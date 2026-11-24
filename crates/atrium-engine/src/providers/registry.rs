// Ported from apps/server/src/orchestration-v2/ProviderAdapterRegistry.ts
// and ProviderAdapterRegistry.test.ts (T3 Code v0.1.11, MIT). The Effect
// instance registry and shared-credential guard are replaced by a plain
// in-memory registry; the lookup, metadata and version-gate cases are kept.
//!
//! Routing a provider instance id to its adapter. Two instances of the same
//! driver are independent: their adapter objects share no mutable state.
//! A version the registry already saw can be stale after an update, so a
//! refused session refreshes the probe before refusing.

use std::sync::Arc;

use crate::contracts::{ProviderCapabilities, ProviderInstanceId};

use super::adapter::{
    AdapterEventSink, OpenSessionInput, ProviderAdapter, ProviderAdapterError,
    ProviderSessionRuntime,
};

/// The minimum Codex version T3 Code 0.1.11 accepts.
pub const MIN_SUPPORTED_VERSION: [u64; 3] = [0, 159, 0];

fn parse_version(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim().split('.');
    Some([
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts
            .next()
            .map(|patch| {
                patch
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse()
                    .unwrap_or(0)
            })
            .unwrap_or(0),
    ])
}

fn version_is_broken(version: &str) -> bool {
    parse_version(version).is_some_and(|version| version < MIN_SUPPORTED_VERSION)
}

#[derive(Debug, thiserror::Error)]
pub enum ProviderAdapterRegistryError {
    #[error("No orchestration provider adapter is registered for {instance_id}.")]
    Lookup { instance_id: String },
    #[error("Failed to read {instance_id} provider metadata.")]
    Metadata { instance_id: String },
    /// Same wording as T3's compatibility gate.
    #[error("{version} does not work with this T3 Code release. Update Codex to >=0.159.0")]
    VersionRefused {
        instance_id: String,
        version: String,
    },
    #[error(transparent)]
    Adapter(#[from] ProviderAdapterError),
}

/// What the registry reports about one instance.
#[derive(Clone, Debug, PartialEq)]
pub struct AdapterMetadata {
    pub driver: String,
    pub continuation_key: String,
    pub enabled: bool,
    pub capabilities: ProviderCapabilities,
}

/// One registered instance. `version` is the last probe; `refresh_version`
/// is consulted only when the last probe rates the version broken.
#[derive(Clone)]
pub struct RegisteredAdapter {
    pub instance_id: ProviderInstanceId,
    pub adapter: Arc<dyn ProviderAdapter>,
    pub continuation_key: String,
    pub enabled: bool,
    pub version: Option<String>,
    pub refresh_version: Option<String>,
}

impl RegisteredAdapter {
    pub fn new(instance_id: impl Into<String>, adapter: Arc<dyn ProviderAdapter>) -> Self {
        let instance_id = ProviderInstanceId::new(instance_id);
        let driver = adapter.driver().to_string();
        Self {
            continuation_key: format!("{driver}:instance:{instance_id}"),
            instance_id,
            adapter,
            enabled: true,
            version: None,
            refresh_version: None,
        }
    }

    pub fn with_version(mut self, version: Option<&str>) -> Self {
        self.version = version.map(str::to_owned);
        self
    }

    /// A single failed probe may predate an update; this is what the next
    /// look would report.
    pub fn with_refreshed_version(mut self, version: Option<&str>) -> Self {
        self.refresh_version = version.map(str::to_owned);
        self
    }

    fn refused_version(&self) -> Option<&str> {
        if self.adapter.checks_version_at_handshake() {
            return None;
        }
        let stale = self
            .version
            .as_deref()
            .filter(|version| version_is_broken(version));
        let stale = stale?;
        match self.refresh_version.as_deref() {
            Some(fresh) if !version_is_broken(fresh) => None,
            Some(fresh) => Some(fresh),
            None => Some(stale),
        }
    }
}

/// An in-memory adapter registry.
#[derive(Default)]
pub struct ProviderAdapterRegistry {
    entries: Vec<RegisteredAdapter>,
}

impl ProviderAdapterRegistry {
    pub fn new(entries: Vec<RegisteredAdapter>) -> Self {
        Self { entries }
    }

    pub fn single(entry: RegisteredAdapter) -> Self {
        Self::new(vec![entry])
    }

    pub fn get(
        &self,
        instance_id: &str,
    ) -> Result<Arc<dyn ProviderAdapter>, ProviderAdapterRegistryError> {
        self.entries
            .iter()
            .find(|entry| entry.instance_id.as_str() == instance_id)
            .map(|entry| entry.adapter.clone())
            .ok_or_else(|| ProviderAdapterRegistryError::Lookup {
                instance_id: instance_id.to_string(),
            })
    }

    pub fn list(&self) -> Vec<ProviderInstanceId> {
        self.entries
            .iter()
            .map(|entry| entry.instance_id.clone())
            .collect()
    }

    pub fn get_metadata(
        &self,
        instance_id: &str,
    ) -> Result<AdapterMetadata, ProviderAdapterRegistryError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.instance_id.as_str() == instance_id)
            .ok_or_else(|| ProviderAdapterRegistryError::Lookup {
                instance_id: instance_id.to_string(),
            })?;
        Ok(AdapterMetadata {
            driver: entry.adapter.driver().to_string(),
            continuation_key: entry.continuation_key.clone(),
            enabled: entry.enabled,
            capabilities: entry.adapter.capabilities(),
        })
    }

    /// Open a session after the version gate. The adapter's own handshake
    /// check is skipped here only when it said it checks at the handshake.
    pub fn open_session(
        &self,
        instance_id: &str,
        input: OpenSessionInput,
        sink: AdapterEventSink,
    ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterRegistryError> {
        let entry = self
            .entries
            .iter()
            .find(|entry| entry.instance_id.as_str() == instance_id)
            .ok_or_else(|| ProviderAdapterRegistryError::Lookup {
                instance_id: instance_id.to_string(),
            })?;
        if let Some(version) = entry.refused_version() {
            return Err(ProviderAdapterRegistryError::VersionRefused {
                instance_id: instance_id.to_string(),
                version: version.to_string(),
            });
        }
        Ok(entry.adapter.open_session(input, sink)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::{ModelSelection, ProviderCapabilities, RuntimeMode, ThreadId};
    use crate::providers::adapter::{AdapterEvent, SelectionTransitionPlan};
    use crate::providers::codex::codex_provider_capabilities;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestAdapter {
        instance_id: ProviderInstanceId,
        capabilities: ProviderCapabilities,
        handshake_check: bool,
        opened: Arc<AtomicBool>,
    }

    impl TestAdapter {
        fn new(instance_id: &str, opened: Arc<AtomicBool>) -> Self {
            Self {
                instance_id: ProviderInstanceId::new(instance_id),
                capabilities: codex_provider_capabilities(),
                handshake_check: false,
                opened,
            }
        }
    }

    impl ProviderAdapter for TestAdapter {
        fn instance_id(&self) -> &ProviderInstanceId {
            &self.instance_id
        }
        fn driver(&self) -> &str {
            "codex"
        }
        fn checks_version_at_handshake(&self) -> bool {
            self.handshake_check
        }
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }
        fn plan_selection_transition(
            &self,
            _current: &ModelSelection,
            _target: &ModelSelection,
        ) -> SelectionTransitionPlan {
            SelectionTransitionPlan::ApplyOnNextTurn
        }
        fn open_session(
            &self,
            _input: OpenSessionInput,
            _sink: AdapterEventSink,
        ) -> Result<Arc<dyn ProviderSessionRuntime>, ProviderAdapterError> {
            self.opened.store(true, Ordering::Relaxed);
            Err(ProviderAdapterError::Unsupported("test adapter".into()))
        }
    }

    fn input() -> OpenSessionInput {
        OpenSessionInput {
            thread_id: ThreadId::new("gated-thread"),
            provider_session_id: crate::contracts::ProviderSessionId::new("gated-session"),
            model_selection: ModelSelection::new("codex_personal", "test-model"),
            runtime_policy: crate::providers::adapter::RuntimePolicy {
                runtime_mode: RuntimeMode::FullAccess,
                interaction_mode: Default::default(),
                cwd: Some("/workspace".into()),
                ..Default::default()
            },
            resume_from_session: None,
            initial_native_thread_id: None,
            now: "2026-11-24T00:00:00.000Z".into(),
        }
    }

    fn sink() -> AdapterEventSink {
        Arc::new(|_event: AdapterEvent| {})
    }

    // Ported from ProviderAdapterRegistry.test.ts, "routes two configured
    // instances of the same driver independently" (T3 Code v0.1.11, MIT).
    #[test]
    fn routes_two_configured_instances_of_the_same_driver_independently() {
        let opened = Arc::new(AtomicBool::new(false));
        let personal = RegisteredAdapter::new(
            "codex_personal",
            Arc::new(TestAdapter::new("codex_personal", opened.clone())),
        );
        let work = RegisteredAdapter::new(
            "codex_work",
            Arc::new(TestAdapter::new("codex_work", opened.clone())),
        );
        let registry = ProviderAdapterRegistry::new(vec![personal, work]);
        assert_eq!(
            registry
                .get("codex_personal")
                .unwrap()
                .instance_id()
                .as_str(),
            "codex_personal"
        );
        assert_eq!(
            registry.get("codex_work").unwrap().instance_id().as_str(),
            "codex_work"
        );
        assert_eq!(
            registry
                .list()
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex_personal", "codex_work"]
        );
        let metadata = registry.get_metadata("codex_work").unwrap();
        assert_eq!(metadata.driver, "codex");
        assert_eq!(metadata.continuation_key, "codex:instance:codex_work");
        assert!(metadata.enabled);
        assert!(
            registry.get("codex_missing").is_err(),
            "an unregistered instance must not silently route"
        );
    }

    // Ported from ProviderAdapterRegistry.test.ts, "refuses a session only
    // when a fresh probe still rates the version broken" (T3 Code v0.1.11,
    // MIT).
    #[test]
    fn refuses_a_session_only_when_a_fresh_probe_still_rates_the_version_broken() {
        let opened = Arc::new(AtomicBool::new(false));
        let adapter = Arc::new(TestAdapter::new("codex_personal", opened.clone()));
        let stale = ProviderAdapterRegistry::single(
            RegisteredAdapter::new("codex_personal", adapter.clone()).with_version(Some("0.100.0")),
        );
        let error = match stale.open_session("codex_personal", input(), sink()) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("the stale version must be refused"),
        };
        assert!(
            error.contains("0.100.0 does not work with this T3 Code release"),
            "{error}"
        );
        assert!(!opened.load(Ordering::Relaxed));

        // The server may have been updated since the last probe.
        let refreshed = ProviderAdapterRegistry::single(
            RegisteredAdapter::new("codex_personal", adapter.clone())
                .with_version(Some("0.100.0"))
                .with_refreshed_version(Some("0.200.0")),
        );
        assert!(matches!(
            refreshed.open_session("codex_personal", input(), sink()),
            Err(ProviderAdapterRegistryError::Adapter(_))
        ));
        assert!(opened.load(Ordering::Relaxed));

        // An adapter that rates the handshake version is not judged on the
        // probe.
        let mut handshake_adapter = TestAdapter::new("codex_personal", opened.clone());
        handshake_adapter.handshake_check = true;
        let handshake = ProviderAdapterRegistry::single(
            RegisteredAdapter::new("codex_personal", Arc::new(handshake_adapter))
                .with_version(Some("0.100.0")),
        );
        assert!(matches!(
            handshake.open_session("codex_personal", input(), sink()),
            Err(ProviderAdapterRegistryError::Adapter(_))
        ));
    }
}
