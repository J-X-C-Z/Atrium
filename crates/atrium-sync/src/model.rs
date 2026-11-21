use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::clock::Hlc;

pub const SYNC_VERSION: u32 = 1;
pub const TOMBSTONE_RETENTION_MS: i64 = 30 * 24 * 60 * 60 * 1_000;

/// The highest clock seen for each device.
pub type Vector = BTreeMap<String, Hlc>;

/// One replicated write. `None` is a tombstone: the key was deleted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub key: String,
    pub value: Option<Value>,
    pub clock: Hlc,
}

/// The current value of one key.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Register {
    pub value: Option<Value>,
    pub clock: Hlc,
}

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("secret values are never replicated: {0}")]
    Secret(String),
    #[error("an overlay key must not be empty")]
    EmptyKey,
}

impl Op {
    pub fn set(key: impl Into<String>, value: Value, clock: Hlc) -> Result<Self, ModelError> {
        let key = key.into();
        validate_key(&key)?;
        validate_value(&key, &value)?;
        Ok(Self {
            key,
            value: Some(value),
            clock,
        })
    }

    pub fn delete(key: impl Into<String>, clock: Hlc) -> Result<Self, ModelError> {
        let key = key.into();
        validate_key(&key)?;
        Ok(Self {
            key,
            value: None,
            clock,
        })
    }

    pub fn is_tombstone(&self) -> bool {
        self.value.is_none()
    }
}

fn validate_key(key: &str) -> Result<(), ModelError> {
    if key.trim().is_empty() {
        return Err(ModelError::EmptyKey);
    }
    if is_secret_key(key) {
        return Err(ModelError::Secret(key.to_string()));
    }
    Ok(())
}

/// Keys that would carry a credential. The brief says only names of
/// environments and MCP servers sync, never their values.
pub fn is_secret_key(key: &str) -> bool {
    let lowered = key.to_ascii_lowercase();
    [
        "/secret",
        "/token",
        "/password",
        "/credential",
        "/authorization",
        "/api-key",
        "/apikey",
        "/private-key",
    ]
    .iter()
    .any(|needle| lowered.ends_with(needle) || lowered.contains(&format!("{needle}/")))
}

/// Rejects values that look like credentials even under an innocuous key.
///
/// This is deliberately conservative: a caller that wants to sync an
/// environment or MCP server should pass a description (`value_set: true`),
/// not the value itself.
pub fn validate_value(key: &str, value: &Value) -> Result<(), ModelError> {
    fn walk(path: &str, value: &Value) -> Result<(), ModelError> {
        match value {
            Value::Object(map) => {
                for (name, child) in map {
                    let lowered = name.to_ascii_lowercase();
                    let secret_name = matches!(
                        lowered.as_str(),
                        "value"
                            | "values"
                            | "env"
                            | "environment"
                            | "headers"
                            | "token"
                            | "secret"
                            | "password"
                            | "api_key"
                            | "apikey"
                            | "authorization"
                            | "credential"
                    );
                    if secret_name && !child.is_boolean() && !child.is_null() {
                        return Err(ModelError::Secret(format!("{path}.{name}")));
                    }
                    walk(&format!("{path}.{name}"), child)?;
                }
                Ok(())
            }
            Value::Array(items) => {
                for (ix, child) in items.iter().enumerate() {
                    walk(&format!("{path}[{ix}]"), child)?;
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    if is_secret_key(key) {
        return Err(ModelError::Secret(key.to_string()));
    }
    walk(key, value)
}

/// `thread/<device>/<backend>/<id>/<field>`.
pub fn thread_key(device: &str, backend: &str, thread: &str, field: &str) -> String {
    format!("thread/{device}/{backend}/{thread}/{field}")
}

/// Parses a thread key back into its parts.
pub fn parse_thread_key(key: &str) -> Option<(&str, &str, &str, &str)> {
    let rest = key.strip_prefix("thread/")?;
    let mut parts = rest.splitn(4, '/');
    let device = parts.next()?;
    let backend = parts.next()?;
    let thread = parts.next()?;
    let field = parts.next()?;
    Some((device, backend, thread, field))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn thread_keys_round_trip() {
        let key = thread_key("studio", "codex", "t-1", "title");
        assert_eq!(
            parse_thread_key(&key),
            Some(("studio", "codex", "t-1", "title"))
        );
    }

    #[test]
    fn secret_keys_and_values_are_refused() {
        let clock = Hlc::new(1, 0, "a");
        assert!(Op::set("env/staging/token", json!("abc"), clock.clone()).is_err());
        assert!(
            Op::set(
                "mcp/github",
                json!({"headers": {"Authorization": "x"}}),
                clock.clone()
            )
            .is_err()
        );
        assert!(
            Op::set(
                "env/staging",
                json!({"name": "staging", "value": "abc"}),
                clock
            )
            .is_err()
        );
    }

    #[test]
    fn names_and_flags_are_allowed() {
        let clock = Hlc::new(1, 0, "a");
        assert!(
            Op::set(
                "env/staging",
                json!({"name": "staging", "value_set": true}),
                clock.clone()
            )
            .is_ok()
        );
        assert!(
            Op::set(
                "mcp/github",
                json!({"name": "github", "value_set": false}),
                clock
            )
            .is_ok()
        );
    }
}
