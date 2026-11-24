//! What one agent session can do.
//!
//! The UI never asks "is this Codex?". It asks this record, filled from the
//! backend's own handshake (Codex `initialize` + `model/list`, ACP
//! `initialize` + `_meta` + config options), and shows only the controls that
//! are true here. See docs/native-parity-implementation.md item 20.
//!
//! `atrium-core` stays IO-free: this module only describes; each client fills
//! the record when it connects.

use serde::{Deserialize, Serialize};

/// One agent session's supported controls.
///
/// The field list is the contract between the clients (which fill it) and the
/// composer, chat header and menus (which read it). Add a field here before
/// adding a control that depends on it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Pasting or attaching images into a turn.
    pub images: bool,
    /// Steering a message into the running turn.
    pub steer: bool,
    /// Holding a message until the running turn ends.
    pub queue: bool,
    /// Stopping the running turn.
    pub interrupt: bool,
    /// A plan-only mode.
    pub plan_mode: bool,
    /// The approval/access modes this agent offers, in the agent's own wire
    /// names, in display order. Empty means the agent has no choice.
    pub approval_modes: Vec<String>,
    /// Choosing a model.
    pub model_choice: bool,
    /// Choosing a reasoning effort.
    pub effort_choice: bool,
    /// Choosing a service tier (fast/priority).
    pub service_tiers: bool,
    /// Forking the conversation.
    pub fork: bool,
    /// Rewinding the agent to an earlier turn.
    pub rewind: bool,
    /// Compacting the context.
    pub compact: bool,
    /// Asking the agent to review a change.
    pub review: bool,
    /// Reporting usage or plan limits.
    pub usage_limits: bool,
    /// Reporting context-window use.
    pub context_window: bool,
    /// Slash commands.
    pub slash_commands: bool,
    /// Skills.
    pub skills: bool,
    /// MCP server status.
    pub mcp_status: bool,
    /// Subagents or child work.
    pub subagents: bool,
}

impl Capabilities {
    /// Nothing beyond a plain text turn.
    pub const NONE: Self = Self {
        images: false,
        steer: false,
        queue: true,
        interrupt: false,
        plan_mode: false,
        approval_modes: Vec::new(),
        model_choice: false,
        effort_choice: false,
        service_tiers: false,
        fork: false,
        rewind: false,
        compact: false,
        review: false,
        usage_limits: false,
        context_window: false,
        slash_commands: false,
        skills: false,
        mcp_status: false,
        subagents: false,
    };

    /// Conservative defaults for a Codex app-server session. `initialize`
    /// refines this; keep it honest if a server offers less.
    pub fn codex() -> Self {
        Self {
            images: true,
            steer: true,
            queue: true,
            interrupt: true,
            plan_mode: true,
            approval_modes: vec!["read-only".into(), "auto".into(), "full-access".into()],
            model_choice: true,
            effort_choice: true,
            service_tiers: true,
            fork: false,
            rewind: true,
            compact: false,
            review: true,
            usage_limits: true,
            context_window: true,
            slash_commands: true,
            skills: false,
            mcp_status: true,
            subagents: false,
        }
    }

    /// Conservative defaults for a Claude Code session. The CLI's
    /// `initialize` reply and `system/init` refine this (models, effort,
    /// slash commands, skills, subagents, MCP status).
    pub fn claude() -> Self {
        Self {
            images: true,
            steer: true,
            queue: true,
            interrupt: true,
            plan_mode: true,
            // Claude's own `--permission-mode` names, in T3's display order.
            approval_modes: vec![
                "default".into(),
                "acceptEdits".into(),
                "plan".into(),
                "bypassPermissions".into(),
            ],
            model_choice: true,
            effort_choice: true,
            service_tiers: false,
            fork: false,
            rewind: false,
            compact: false,
            review: false,
            usage_limits: true,
            context_window: true,
            slash_commands: true,
            skills: true,
            mcp_status: true,
            subagents: true,
        }
    }

    /// Conservative defaults for an ACP session. The session's `initialize`
    /// capabilities, `_meta` and config options refine this.
    pub fn acp() -> Self {
        Self {
            images: true,
            steer: false,
            queue: true,
            interrupt: true,
            plan_mode: false,
            approval_modes: Vec::new(),
            model_choice: false,
            effort_choice: false,
            service_tiers: false,
            fork: false,
            rewind: false,
            compact: false,
            review: false,
            usage_limits: false,
            context_window: false,
            slash_commands: true,
            skills: false,
            mcp_status: false,
            subagents: false,
        }
    }

    /// Every control on, for tests and mocks only.
    pub fn everything() -> Self {
        Self {
            images: true,
            steer: true,
            queue: true,
            interrupt: true,
            plan_mode: true,
            approval_modes: vec!["auto".into()],
            model_choice: true,
            effort_choice: true,
            service_tiers: true,
            fork: true,
            rewind: true,
            compact: true,
            review: true,
            usage_limits: true,
            context_window: true,
            slash_commands: true,
            skills: true,
            mcp_status: true,
            subagents: true,
        }
    }

    /// True when the agent offers any approval/access choice.
    pub fn has_approval_choice(&self) -> bool {
        !self.approval_modes.is_empty()
    }
}

impl Default for Capabilities {
    fn default() -> Self {
        Self::NONE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_plain_text_only() {
        let caps = Capabilities::NONE;
        assert!(caps.queue, "the app can always queue for a plain agent");
        assert!(!caps.steer && !caps.images && !caps.approval_modes.iter().any(|_| true));
        assert!(!caps.has_approval_choice());
    }

    #[test]
    fn codex_and_acp_are_not_the_same() {
        assert!(Capabilities::codex().has_approval_choice());
        assert!(!Capabilities::acp().has_approval_choice());
        assert!(Capabilities::codex().steer);
        assert!(!Capabilities::acp().steer);
    }

    #[test]
    fn claude_offers_its_own_permission_modes_and_steering() {
        let caps = Capabilities::claude();
        assert!(caps.steer && caps.plan_mode && caps.subagents);
        assert_eq!(
            caps.approval_modes,
            ["default", "acceptEdits", "plan", "bypassPermissions"]
        );
        assert!(!caps.service_tiers);
    }

    #[test]
    fn everything_has_every_flag() {
        let caps = Capabilities::everything();
        assert!(caps.images && caps.skills && caps.subagents && caps.compact);
    }
}
