//! Which controls a backend offers, for the UI.
//!
//! Item 20 fills this from the live session's own handshake. Until then the
//! conservative per-backend defaults keep every control honest: a backend
//! that has not declared a feature does not get its button.

use atrium_core::Capabilities;

use crate::agents::split_backend;

/// The capabilities to use for `backend` (`codex`, `claude@studio`, ...).
///
/// Callers should pass what the running session reported when they have it
/// ([`Capabilities`] on the workspace); this is the fallback for a backend
/// that has not connected yet.
pub fn for_backend(backend: &str) -> Capabilities {
    match split_backend(backend).0 {
        "codex" => Capabilities::codex(),
        "claude" | "deepseek" | "antigravity" | "kiro" | "opencode" | "cursor" => {
            Capabilities::acp()
        }
        _ => Capabilities::NONE,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_backends_get_defaults_and_unknown_gets_none() {
        assert!(for_backend("codex").model_choice);
        assert!(for_backend("claude@studio").slash_commands);
        assert!(for_backend("antigravity").queue);
        assert_eq!(for_backend("mystery"), Capabilities::NONE);
    }
}
