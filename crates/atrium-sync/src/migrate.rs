use serde_json::{Value, json};

use crate::{
    clock::{Hlc, HybridClock},
    model::{Op, thread_key},
    store::now_ms,
};

/// The parts of Atrium's settings that become overlay entries.
///
/// They are read from the app's settings file, but this crate does not depend
/// on the app: it takes only the metadata that is safe to replicate.
#[derive(Clone, Debug, Default)]
pub struct SettingsView {
    /// Thread ids pinned on this device.
    pub pinned: Vec<String>,
    /// ACP sessions hidden on this device.
    pub hidden: Vec<String>,
    /// Project folders, as local paths. They are stable only on this device;
    /// the project id is derived from the path here and refined by the app
    /// once it has a git identity.
    pub projects: Vec<String>,
    /// Environment names, never their values.
    pub environments: Vec<String>,
    /// MCP server names, never their environment or headers.
    pub mcp_servers: Vec<String>,
}

/// Turns existing per-device settings into overlay ops stamped with this
/// device. The caller merges them once, on first start after upgrading.
pub fn migrate_settings(device: &str, backend: &str, view: &SettingsView, at_ms: i64) -> Vec<Op> {
    let mut clock = HybridClock::new(device, Hlc::default());
    let stamp = if at_ms > 0 { at_ms } else { now_ms() };
    let mut ops = Vec::new();
    let mut push = |key: String, value: Value| {
        if let Ok(op) = Op::set(key, value, clock.tick(stamp)) {
            ops.push(op);
        }
    };
    for thread in &view.pinned {
        push(thread_key(device, backend, thread, "pinned"), json!(true));
    }
    for thread in &view.hidden {
        push(thread_key(device, backend, thread, "hidden"), json!(true));
    }
    for (ix, project) in view.projects.iter().enumerate() {
        push(
            format!("project/{ix}/path"),
            json!({"path": project, "device": device}),
        );
    }
    for name in &view.environments {
        push(format!("env/{name}"), json!({"name": name}));
    }
    for name in &view.mcp_servers {
        push(format!("mcp/{name}"), json!({"name": name}));
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_keeps_names_and_drops_nothing_secret() {
        let view = SettingsView {
            pinned: vec!["t1".into()],
            hidden: vec!["s1".into()],
            projects: vec!["/code/a".into()],
            environments: vec!["staging".into()],
            mcp_servers: vec!["github".into()],
        };
        let ops = migrate_settings("me", "codex", &view, 1_000);
        assert_eq!(ops.len(), 5);
        assert!(ops.iter().all(|op| op.value.is_some()));
        assert!(ops.iter().any(|op| op.key == "thread/me/codex/t1/pinned"));
        assert!(ops.iter().any(|op| op.key == "env/staging"));
        assert!(ops.iter().any(|op| op.key == "mcp/github"));
    }

    #[test]
    fn migration_is_deterministic_for_the_same_device() {
        let view = SettingsView {
            pinned: vec!["t1".into()],
            ..SettingsView::default()
        };
        let first = migrate_settings("me", "codex", &view, 5_000);
        let second = migrate_settings("me", "codex", &view, 5_000);
        assert_eq!(first, second);
    }
}
