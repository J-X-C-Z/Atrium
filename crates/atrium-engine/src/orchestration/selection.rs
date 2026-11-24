// Ported from apps/server/src/orchestration-v2/ProviderSessionTransitionPolicy.ts
// and apps/server/src/orchestration-v2/ProviderSwitchService.ts (T3 Code v0.1.11, MIT).

//! What happens to a live provider session when the thread's model selection
//! changes: apply it on the next turn, restart the session, create a new one
//! with a context handoff, or reject the change.
//!
//! Both decisions are pure. The adapter registry is a lookup trait so tests
//! can supply metadata and a selection-transition classification without a
//! provider process.

use super::types::{
    InteractionMode, ModelSelection, Projection, ProviderCapabilities, ProviderDriverKind,
    ProviderInstanceId, ProviderSession, ProviderSessionId, ProviderSessionStatus, RunStatus,
    RuntimeMode, model_selections_equal, parse_iso,
};

/// The adapter's plan for a selection change inside one provider instance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSelectionTransition {
    ApplyOnNextTurn,
    RestartSession,
    CreateWithHandoff,
    Reject { reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderSessionTransition {
    Reuse,
    SwitchModelInSession,
    RestartAndResume,
    CreateWithHandoff,
    Reject { reason: String },
}

impl ProviderSessionTransition {
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Reuse => "reuse",
            Self::SwitchModelInSession => "switch_model_in_session",
            Self::RestartAndResume => "restart_and_resume",
            Self::CreateWithHandoff => "create_with_handoff",
            Self::Reject { .. } => "reject",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderContinuationIdentity {
    pub driver_kind: ProviderDriverKind,
    pub continuation_key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSessionTransitionState {
    pub driver: ProviderDriverKind,
    pub continuation_identity: ProviderContinuationIdentity,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub workspace: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderSessionTransitionTarget {
    pub state: ProviderSessionTransitionState,
    pub available: bool,
}

/// Port of `decideProviderSessionTransition`.
pub fn decide_provider_session_transition(
    current: Option<&ProviderSessionTransitionState>,
    target: &ProviderSessionTransitionTarget,
    selection_transition: Option<&ProviderSelectionTransition>,
) -> ProviderSessionTransition {
    if !target.available {
        return ProviderSessionTransition::Reject {
            reason: "The target provider instance is unavailable.".into(),
        };
    }
    let Some(current) = current else {
        return ProviderSessionTransition::CreateWithHandoff;
    };
    let target = &target.state;
    let continuation_compatible = current.continuation_identity.driver_kind
        == target.continuation_identity.driver_kind
        && current.continuation_identity.continuation_key
            == target.continuation_identity.continuation_key;
    if !continuation_compatible || current.driver != target.driver {
        return ProviderSessionTransition::CreateWithHandoff;
    }

    let instance_changed =
        current.model_selection.instance_id != target.model_selection.instance_id;
    let runtime_changed = current.runtime_mode != target.runtime_mode;
    let workspace_changed = current.workspace != target.workspace;
    let selection_changed =
        !model_selections_equal(&current.model_selection, &target.model_selection);
    if selection_changed && !instance_changed {
        match selection_transition {
            Some(ProviderSelectionTransition::CreateWithHandoff) => {
                return ProviderSessionTransition::CreateWithHandoff;
            }
            Some(ProviderSelectionTransition::Reject { reason }) => {
                return ProviderSessionTransition::Reject {
                    reason: reason.clone(),
                };
            }
            None => {
                return ProviderSessionTransition::Reject {
                    reason: "The provider adapter did not classify the selection change.".into(),
                };
            }
            Some(
                ProviderSelectionTransition::ApplyOnNextTurn
                | ProviderSelectionTransition::RestartSession,
            ) => {}
        }
    }
    if instance_changed || runtime_changed || workspace_changed {
        return ProviderSessionTransition::RestartAndResume;
    }
    if selection_changed {
        return match selection_transition {
            Some(ProviderSelectionTransition::ApplyOnNextTurn) => {
                ProviderSessionTransition::SwitchModelInSession
            }
            Some(ProviderSelectionTransition::RestartSession) => {
                ProviderSessionTransition::RestartAndResume
            }
            _ => ProviderSessionTransition::Reuse,
        };
    }
    // Apply the interaction mode on the next turn.
    ProviderSessionTransition::Reuse
}

/// The metadata the switch planner reads for one provider instance.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderInstanceMetadata {
    pub driver: ProviderDriverKind,
    pub continuation_key: String,
    pub enabled: bool,
    pub capabilities: ProviderCapabilities,
}

/// Adapter lookups a provider switch needs.
pub trait ProviderSwitchRegistry {
    fn metadata(&self, instance_id: &ProviderInstanceId) -> Option<ProviderInstanceMetadata>;

    /// Classify a model change inside one instance. The default is a restart.
    fn plan_selection_transition(
        &self,
        _current: &ModelSelection,
        _target: &ModelSelection,
        _session_capabilities: &ProviderCapabilities,
    ) -> ProviderSelectionTransition {
        ProviderSelectionTransition::RestartSession
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSwitchPlan {
    pub instance_changed: bool,
    pub release_provider_session_ids: Vec<ProviderSessionId>,
    pub transition: ProviderSessionTransition,
}

impl ProviderSwitchPlan {
    /// A new provider conversation needs the old thread's context carried over.
    pub fn needs_context_handoff(&self) -> bool {
        matches!(
            self.transition,
            ProviderSessionTransition::CreateWithHandoff
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ProviderSwitchPlanError {
    pub message: String,
}

impl std::fmt::Display for ProviderSwitchPlanError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ProviderSwitchPlanError {}

fn is_live(session: &ProviderSession) -> bool {
    !matches!(
        session.status,
        ProviderSessionStatus::Stopped | ProviderSessionStatus::Error
    )
}

/// Port of `ProviderSwitchService.plan`.
pub fn plan_provider_switch<R: ProviderSwitchRegistry>(
    projection: &Projection,
    target_model_selection: &ModelSelection,
    registry: &R,
) -> Result<ProviderSwitchPlan, ProviderSwitchPlanError> {
    let thread = &projection.thread;
    let current = &thread.model_selection;
    let instance_changed = current.instance_id != target_model_selection.instance_id;

    let mut current_sessions: Vec<&ProviderSession> = projection
        .provider_sessions
        .iter()
        .filter(|session| session.provider_instance_id == current.instance_id)
        .collect();
    current_sessions.sort_by_key(|session| std::cmp::Reverse(parse_iso(&session.updated_at)));
    let current_session = current_sessions
        .iter()
        .copied()
        .find(|session| is_live(session));
    let negotiated_capabilities = current_session
        .map(|session| session.capabilities.clone())
        .or_else(|| {
            current_sessions
                .first()
                .map(|session| session.capabilities.clone())
        });
    let current_provider_thread = projection.provider_threads.iter().find(|provider_thread| {
        thread.active_provider_thread_id.as_ref() == Some(&provider_thread.id)
            && provider_thread.provider_instance_id == current.instance_id
            && provider_thread.native_thread_ref.is_some()
    });

    let current_metadata = registry.metadata(&current.instance_id);
    let target_metadata = registry.metadata(&target_model_selection.instance_id);

    let selection_transition = (current.instance_id == target_model_selection.instance_id
        && !model_selections_equal(current, target_model_selection)
        && target_metadata.is_some()
        && current_metadata.is_some()
        && (current_session.is_some() || current_provider_thread.is_some()))
    .then(|| match &negotiated_capabilities {
        None => ProviderSelectionTransition::ApplyOnNextTurn,
        Some(capabilities) => {
            registry.plan_selection_transition(current, target_model_selection, capabilities)
        }
    });

    let workspace = thread
        .worktree_path
        .clone()
        .or_else(|| current_session.map(|session| session.cwd.clone()))
        .unwrap_or_else(|| "<unresolved-workspace>".into());

    let Some(target_metadata) = target_metadata else {
        return Err(ProviderSwitchPlanError {
            message: "The target provider instance is unavailable.".into(),
        });
    };

    let current_state = match current_metadata {
        Some(metadata) if current_session.is_some() || current_provider_thread.is_some() => {
            Some(ProviderSessionTransitionState {
                driver: metadata.driver.clone(),
                continuation_identity: ProviderContinuationIdentity {
                    driver_kind: metadata.driver,
                    continuation_key: metadata.continuation_key,
                },
                model_selection: current.clone(),
                runtime_mode: thread.runtime_mode,
                interaction_mode: thread.interaction_mode,
                workspace: workspace.clone(),
            })
        }
        _ => None,
    };
    let target = ProviderSessionTransitionTarget {
        state: ProviderSessionTransitionState {
            driver: target_metadata.driver.clone(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: target_metadata.driver,
                continuation_key: target_metadata.continuation_key,
            },
            model_selection: target_model_selection.clone(),
            runtime_mode: thread.runtime_mode,
            interaction_mode: thread.interaction_mode,
            workspace,
        },
        available: target_metadata.enabled,
    };
    let transition = decide_provider_session_transition(
        current_state.as_ref(),
        &target,
        selection_transition.as_ref(),
    );
    if let ProviderSessionTransition::Reject { reason } = &transition {
        return Err(ProviderSwitchPlanError {
            message: reason.clone(),
        });
    }
    let release_provider_session_ids = match &transition {
        ProviderSessionTransition::RestartAndResume => current_session
            .map(|session| vec![session.id.clone()])
            .unwrap_or_default(),
        ProviderSessionTransition::CreateWithHandoff => projection
            .provider_sessions
            .iter()
            .filter(|session| {
                is_live(session)
                    && session.provider_instance_id != target_model_selection.instance_id
            })
            .map(|session| session.id.clone())
            .collect(),
        _ => Vec::new(),
    };
    Ok(ProviderSwitchPlan {
        instance_changed,
        release_provider_session_ids,
        transition,
    })
}

/// A provider switch is applied at a turn boundary; a waiting run is already
/// past its boundary.
pub fn provider_switch_waits_for_run(run: &RunStatus) -> bool {
    matches!(
        run,
        RunStatus::Preparing | RunStatus::Starting | RunStatus::Running | RunStatus::Waiting
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestration::types::{
        Actor, CreationSource, ProviderThread, ProviderThreadStatus, iso,
    };

    fn thread(model: ModelSelection) -> super::super::types::Thread {
        super::super::types::Thread {
            id: "thread:switch".into(),
            model_selection: model,
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            worktree_path: Some("/repo".into()),
            created_by: Actor::User,
            creation_source: CreationSource::Web,
            ..super::super::types::Thread::default()
        }
    }

    fn session(
        id: &str,
        instance: &str,
        status: ProviderSessionStatus,
        updated_at: &str,
        capabilities: ProviderCapabilities,
    ) -> ProviderSession {
        ProviderSession {
            id: id.into(),
            driver: "codex".into(),
            provider_instance_id: instance.into(),
            status,
            cwd: "/repo".into(),
            model: Some("gpt".into()),
            capabilities,
            created_at: iso(0),
            updated_at: updated_at.into(),
            ..ProviderSession::default()
        }
    }

    fn registration(driver: &str, continuation_key: &str) -> ProviderInstanceMetadata {
        ProviderInstanceMetadata {
            driver: driver.into(),
            continuation_key: continuation_key.into(),
            enabled: true,
            capabilities: ProviderCapabilities::default(),
        }
    }

    #[derive(Default)]
    struct Registry {
        metadata: std::collections::BTreeMap<String, ProviderInstanceMetadata>,
        selection: Option<ProviderSelectionTransition>,
    }

    impl ProviderSwitchRegistry for Registry {
        fn metadata(&self, instance_id: &ProviderInstanceId) -> Option<ProviderInstanceMetadata> {
            self.metadata.get(instance_id.as_str()).cloned()
        }

        fn plan_selection_transition(
            &self,
            _current: &ModelSelection,
            _target: &ModelSelection,
            _session_capabilities: &ProviderCapabilities,
        ) -> ProviderSelectionTransition {
            self.selection
                .clone()
                .unwrap_or(ProviderSelectionTransition::RestartSession)
        }
    }

    fn registry(entries: &[(&str, &str)]) -> Registry {
        Registry {
            metadata: entries
                .iter()
                .map(|(instance, key)| ((*instance).to_owned(), registration("codex", key)))
                .collect(),
            selection: None,
        }
    }

    // T3 `ProviderSessionTransitionPolicy.test.ts`.
    #[test]
    fn reuses_compatible_sessions_and_treats_interaction_mode_as_turn_scoped() {
        let base = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:one".into(),
            },
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: "/repo".into(),
        };
        let mut target = base.clone();
        target.interaction_mode = InteractionMode::Plan;
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &ProviderSessionTransitionTarget {
                    state: target,
                    available: true
                },
                None
            ),
            ProviderSessionTransition::Reuse
        );
    }

    #[test]
    fn uses_the_adapters_selection_transition_classification() {
        let base = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:one".into(),
            },
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: "/repo".into(),
        };
        let mut target = base.clone();
        target.model_selection = ModelSelection::new("codex", "gpt-5.2-codex");
        let target = ProviderSessionTransitionTarget {
            state: target,
            available: true,
        };
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target,
                Some(&ProviderSelectionTransition::ApplyOnNextTurn)
            ),
            ProviderSessionTransition::SwitchModelInSession
        );
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target,
                Some(&ProviderSelectionTransition::RestartSession)
            ),
            ProviderSessionTransition::RestartAndResume
        );
    }

    #[test]
    fn restarts_compatible_instances_for_workspace_or_runtime_changes() {
        let base = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:one".into(),
            },
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: "/repo".into(),
        };
        let mut target = base.clone();
        target.workspace = "/other".into();
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &ProviderSessionTransitionTarget {
                    state: target,
                    available: true
                },
                None
            ),
            ProviderSessionTransition::RestartAndResume
        );
    }

    #[test]
    fn preserves_handoff_and_missing_classification_outcomes_across_workspace_changes() {
        let base = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:one".into(),
            },
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: "/repo".into(),
        };
        let mut target_state = base.clone();
        target_state.model_selection = ModelSelection::new("codex", "gpt-5.2-codex");
        target_state.workspace = "/other".into();
        let target = ProviderSessionTransitionTarget {
            state: target_state,
            available: true,
        };
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &target,
                Some(&ProviderSelectionTransition::CreateWithHandoff)
            ),
            ProviderSessionTransition::CreateWithHandoff
        );
        assert_eq!(
            decide_provider_session_transition(Some(&base), &target, None),
            ProviderSessionTransition::Reject {
                reason: "The provider adapter did not classify the selection change.".into()
            }
        );
    }

    #[test]
    fn uses_portable_handoff_for_incompatible_continuation_identities() {
        let base = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:one".into(),
            },
            model_selection: ModelSelection::new("codex", "gpt-5.1-codex"),
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            workspace: "/repo".into(),
        };
        let target = ProviderSessionTransitionState {
            driver: "codex".into(),
            continuation_identity: ProviderContinuationIdentity {
                driver_kind: "codex".into(),
                continuation_key: "codex:account:other".into(),
            },
            model_selection: ModelSelection::new("codex_other", "gpt-5.1-codex"),
            ..base.clone()
        };
        assert_eq!(
            decide_provider_session_transition(
                Some(&base),
                &ProviderSessionTransitionTarget {
                    state: target,
                    available: true
                },
                None
            ),
            ProviderSessionTransition::CreateWithHandoff
        );
    }

    // T3 `ProviderSwitchService.test.ts`.
    #[test]
    fn releases_the_newest_live_session_not_the_newest_record_overall() {
        let mut projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        projection.provider_sessions = vec![
            session(
                "session_primary",
                "codex_primary",
                ProviderSessionStatus::Ready,
                "2026-06-20T00:00:00Z",
                ProviderCapabilities::default(),
            ),
            session(
                "session_newer_live",
                "codex_primary",
                ProviderSessionStatus::Ready,
                "2026-06-20T00:00:01Z",
                ProviderCapabilities::default(),
            ),
            session(
                "dead_session",
                "codex_primary",
                ProviderSessionStatus::Stopped,
                "2026-06-20T00:00:02Z",
                ProviderCapabilities::default(),
            ),
        ];
        let plan = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_primary", "gpt-5.2-codex"),
            &registry(&[("codex_primary", "codex:account:primary")]),
        )
        .expect("plan");
        assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
        assert_eq!(
            plan.release_provider_session_ids,
            vec![ProviderSessionId::new("session_newer_live")]
        );
    }

    #[test]
    fn creates_a_fresh_session_with_handoff_when_every_recorded_session_is_dead() {
        let mut projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        projection.provider_sessions = vec![
            session(
                "dead_older",
                "codex_primary",
                ProviderSessionStatus::Stopped,
                "2026-06-20T00:00:00Z",
                ProviderCapabilities::default(),
            ),
            session(
                "dead_newer",
                "codex_primary",
                ProviderSessionStatus::Error,
                "2026-06-20T00:00:02Z",
                ProviderCapabilities::default(),
            ),
        ];
        let plan = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_primary", "gpt-5.2-codex"),
            &registry(&[("codex_primary", "codex:account:primary")]),
        )
        .expect("plan");
        assert_eq!(
            plan.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
        assert!(plan.release_provider_session_ids.is_empty());
        assert!(plan.needs_context_handoff());
    }

    #[test]
    fn falls_back_to_the_native_provider_thread_when_every_session_is_dead() {
        let mut projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        projection.thread.active_provider_thread_id = Some("provider-thread:native".into());
        projection.provider_sessions = vec![session(
            "dead",
            "codex_primary",
            ProviderSessionStatus::Stopped,
            "2026-06-20T00:00:00Z",
            ProviderCapabilities::default(),
        )];
        projection.provider_threads = vec![ProviderThread {
            id: "provider-thread:native".into(),
            driver: "codex".into(),
            provider_instance_id: "codex_primary".into(),
            provider_session_id: Some("dead".into()),
            app_thread_id: Some("thread:switch".into()),
            native_thread_ref: Some(crate::contracts::ProviderRef {
                driver: "codex".into(),
                native_id: Some("native-thread:abc".into()),
                strength: crate::contracts::NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            status: ProviderThreadStatus::Idle,
            ..ProviderThread::default()
        }];
        let plan = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_primary", "gpt-5.2-codex"),
            &registry(&[("codex_primary", "codex:account:primary")]),
        )
        .expect("plan");
        assert_eq!(plan.transition, ProviderSessionTransition::RestartAndResume);
        assert!(plan.release_provider_session_ids.is_empty());
    }

    #[test]
    fn hands_off_from_a_native_provider_thread_after_its_session_detaches() {
        let mut projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        projection.thread.active_provider_thread_id = Some("provider-thread:native".into());
        projection.provider_threads = vec![ProviderThread {
            id: "provider-thread:native".into(),
            driver: "codex".into(),
            provider_instance_id: "codex_primary".into(),
            app_thread_id: Some("thread:switch".into()),
            native_thread_ref: Some(crate::contracts::ProviderRef {
                driver: "codex".into(),
                native_id: Some("native-thread:abc".into()),
                strength: crate::contracts::NativeRefStrength::Strong,
                fingerprint: None,
                ordinal: None,
            }),
            status: ProviderThreadStatus::Idle,
            ..ProviderThread::default()
        }];
        let plan = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_other", "gpt-5.2-codex"),
            &registry(&[
                ("codex_primary", "codex:account:primary"),
                ("codex_other", "codex:account:other"),
            ]),
        )
        .expect("plan");
        assert_eq!(
            plan.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
        assert!(plan.release_provider_session_ids.is_empty());
    }

    #[test]
    fn distinguishes_compatible_and_incompatible_instances_of_the_same_driver() {
        let mut projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        projection.provider_sessions = vec![session(
            "session_primary",
            "codex_primary",
            ProviderSessionStatus::Ready,
            "2026-06-20T00:00:00Z",
            ProviderCapabilities::default(),
        )];
        let reg = registry(&[
            ("codex_primary", "codex:account:primary"),
            ("codex_compatible", "codex:account:primary"),
            ("codex_incompatible", "codex:account:other"),
        ]);
        let compatible = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_compatible", "gpt-5.1-codex"),
            &reg,
        )
        .expect("compatible");
        let incompatible = plan_provider_switch(
            &projection,
            &ModelSelection::new("codex_incompatible", "gpt-5.1-codex"),
            &reg,
        )
        .expect("incompatible");
        assert_eq!(
            compatible.transition,
            ProviderSessionTransition::RestartAndResume
        );
        assert_eq!(
            incompatible.transition,
            ProviderSessionTransition::CreateWithHandoff
        );
    }

    #[test]
    fn rejects_an_unavailable_target_instance() {
        let projection = Projection::new(thread(ModelSelection::new(
            "codex_primary",
            "gpt-5.1-codex",
        )));
        let error = plan_provider_switch(
            &projection,
            &ModelSelection::new("missing", "gpt-5.2-codex"),
            &registry(&[("codex_primary", "codex:account:primary")]),
        )
        .expect_err("unavailable");
        assert_eq!(
            error.message,
            "The target provider instance is unavailable."
        );
    }

    #[test]
    fn a_switch_never_lands_mid_run() {
        assert!(provider_switch_waits_for_run(&RunStatus::Running));
        assert!(!provider_switch_waits_for_run(&RunStatus::Queued));
    }
}
