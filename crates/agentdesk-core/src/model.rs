use serde::{Deserialize, Serialize};

/// The id of a request the backend made to us (such as an approval), echoed
/// back verbatim in the answer. JSON-RPC allows numbers or strings.
pub type RequestId = serde_json::Value;

/// A conversation as listed in the sidebar.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadMeta {
    pub id: String,
    /// Which agent the thread belongs to, such as `codex` or `claude`.
    pub backend: String,
    /// User-facing title; falls back to `preview` when the backend has none.
    pub name: Option<String>,
    /// Usually the first user message.
    pub preview: String,
    pub cwd: String,
    pub model: Option<String>,
    /// Unix seconds.
    pub created_at: i64,
    /// Unix seconds.
    pub updated_at: i64,
}

impl ThreadMeta {
    pub fn title(&self) -> String {
        let pick = self
            .name
            .as_deref()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(self.preview.trim());
        let line = pick.lines().next().unwrap_or("").trim();
        if line.is_empty() {
            "New chat".to_string()
        } else if line.chars().count() > 60 {
            let cut: String = line.chars().take(60).collect();
            format!("{cut}…")
        } else {
            line.to_string()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnStatus {
    InProgress,
    Completed,
    Interrupted,
    Failed,
}

/// Where one item of a turn stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Delete,
    Update { move_to: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub kind: ChangeKind,
    pub diff: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    pub step: String,
    pub done: bool,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub total_tokens: u64,
    pub last_turn_tokens: u64,
    pub context_window: Option<u64>,
}

/// One thing that happened in a turn, normalized across backends.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    User {
        id: String,
        text: String,
        /// Paths of attached images and mentioned files.
        attachments: Vec<String>,
    },
    Agent {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        text: String,
    },
    Command {
        id: String,
        command: String,
        cwd: String,
        status: ItemStatus,
        output: String,
        exit_code: Option<i32>,
        duration_ms: Option<u64>,
    },
    FileChange {
        id: String,
        changes: Vec<FileChange>,
        status: ItemStatus,
    },
    Tool {
        id: String,
        /// Such as `server.tool` for MCP.
        name: String,
        status: ItemStatus,
        arguments: String,
        result: String,
        duration_ms: Option<u64>,
    },
    WebSearch {
        id: String,
        query: String,
        /// What else the search did, such as a page it opened.
        detail: Option<String>,
    },
    /// An image the agent generated.
    Image {
        id: String,
        status: ItemStatus,
        /// Where the image was saved, once it was.
        path: Option<String>,
        prompt: Option<String>,
    },
    /// The agent started or talked to a sub-agent.
    SubAgent {
        id: String,
        label: String,
        status: ItemStatus,
        detail: Option<String>,
    },
    /// Something the backend reported that we do not draw specially yet.
    Other {
        id: String,
        label: String,
    },
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::User { id, .. }
            | Item::Agent { id, .. }
            | Item::Reasoning { id, .. }
            | Item::Command { id, .. }
            | Item::FileChange { id, .. }
            | Item::Tool { id, .. }
            | Item::WebSearch { id, .. }
            | Item::Image { id, .. }
            | Item::SubAgent { id, .. }
            | Item::Other { id, .. } => id,
        }
    }
}

/// Where a long-running goal stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalStatus {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
}

impl GoalStatus {
    pub fn label(self) -> &'static str {
        match self {
            GoalStatus::Active => "Working",
            GoalStatus::Paused => "Paused",
            GoalStatus::Blocked => "Blocked",
            GoalStatus::UsageLimited => "Usage limit reached",
            GoalStatus::BudgetLimited => "Budget used up",
            GoalStatus::Complete => "Complete",
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            GoalStatus::Active => "active",
            GoalStatus::Paused => "paused",
            GoalStatus::Blocked => "blocked",
            GoalStatus::UsageLimited => "usageLimited",
            GoalStatus::BudgetLimited => "budgetLimited",
            GoalStatus::Complete => "complete",
        }
    }

    pub fn from_wire(text: &str) -> Option<Self> {
        Some(match text {
            "active" => GoalStatus::Active,
            "paused" => GoalStatus::Paused,
            "blocked" => GoalStatus::Blocked,
            "usageLimited" => GoalStatus::UsageLimited,
            "budgetLimited" => GoalStatus::BudgetLimited,
            "complete" => GoalStatus::Complete,
            _ => return None,
        })
    }
}

/// An objective the agent keeps working toward across turns, within a budget.
#[derive(Clone, Debug, PartialEq)]
pub struct Goal {
    pub objective: String,
    pub status: GoalStatus,
    pub token_budget: Option<u64>,
    pub tokens_used: u64,
    pub time_used_secs: u64,
}

/// One usage window, such as the five-hour or the weekly limit.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RateWindow {
    pub used_percent: f64,
    pub window_mins: Option<u64>,
    /// Unix seconds.
    pub resets_at: Option<i64>,
}

impl RateWindow {
    /// "5-hour", "Weekly", or the window in hours or days.
    pub fn label(&self) -> String {
        match self.window_mins {
            Some(300) => "5-hour limit".into(),
            Some(10080) => "Weekly limit".into(),
            Some(mins) if mins % 1440 == 0 => format!("{}-day limit", mins / 1440),
            Some(mins) if mins % 60 == 0 => format!("{}-hour limit", mins / 60),
            Some(mins) => format!("{mins}-minute limit"),
            None => "Usage limit".into(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Credits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

/// How much of the subscription's included usage is spent.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RateLimits {
    pub name: Option<String>,
    pub primary: Option<RateWindow>,
    pub secondary: Option<RateWindow>,
    pub credits: Option<Credits>,
    pub plan: Option<String>,
    /// A limit is reached and usage is paused until it resets.
    pub reached: bool,
}

impl RateLimits {
    /// Merges a sparse update: fields it lacks keep their last value.
    pub fn merge(&mut self, update: RateLimits) {
        if update.name.is_some() {
            self.name = update.name;
        }
        if update.primary.is_some() {
            self.primary = update.primary;
        }
        if update.secondary.is_some() {
            self.secondary = update.secondary;
        }
        if update.credits.is_some() {
            self.credits = update.credits;
        }
        if update.plan.is_some() {
            self.plan = update.plan;
        }
        self.reached = update.reached;
    }

    /// The fullest window's share used, 0 to 100.
    pub fn highest_used(&self) -> Option<f64> {
        [self.primary, self.secondary]
            .into_iter()
            .flatten()
            .map(|window| window.used_percent)
            .reduce(f64::max)
    }
}

/// Who Codex is signed in as.
#[derive(Clone, Debug, PartialEq)]
pub enum Account {
    ApiKey,
    ChatGpt {
        email: Option<String>,
        plan: String,
        workspace: Option<String>,
    },
    Other(String),
}

impl Account {
    /// "Pro", "Plus", "Team"… for a ChatGPT plan.
    pub fn plan_label(&self) -> Option<String> {
        let Account::ChatGpt { plan, .. } = self else {
            return None;
        };
        Some(plan_label(plan))
    }
}

/// A plan id as people say it: `pro` → "Pro", `prolite` → "Pro Lite".
pub fn plan_label(plan: &str) -> String {
    match plan {
        "prolite" => "Pro Lite".into(),
        "promax" => "Pro Max".into(),
        "edu_plus" => "Edu Plus".into(),
        "edu_pro" => "Edu Pro".into(),
        "sci_plus" => "Sci Plus".into(),
        "self_serve_business_prolite" => "Business Pro Lite".into(),
        "self_serve_business_usage_based" | "business" => "Business".into(),
        plan if plan.starts_with("ent") => "Enterprise".into(),
        plan => {
            let mut chars = plan.chars();
            chars
                .next()
                .map(|c| c.to_uppercase().chain(chars).collect())
                .unwrap_or_default()
        }
    }
}

/// One question the agent asks, with the choices it offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Question {
    pub id: String,
    pub header: String,
    pub question: String,
    /// `(label, description)` pairs.
    pub options: Vec<(String, String)>,
    /// Free text is allowed besides the options.
    pub allow_other: bool,
    pub secret: bool,
}

/// One choice of a [`ConfigOption`].
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigChoice {
    pub value: String,
    pub name: String,
    pub description: Option<String>,
    /// The group it is listed under, such as a model provider.
    pub group: Option<String>,
}

/// A setting an agent offers per session, such as its mode, model or
/// reasoning level (ACP `configOptions`, and session modes).
#[derive(Clone, Debug, PartialEq)]
pub struct ConfigOption {
    pub id: String,
    pub name: String,
    /// `mode`, `model`, `thought_level`, or something the agent made up.
    pub category: Option<String>,
    pub current: String,
    pub choices: Vec<ConfigChoice>,
}

impl ConfigOption {
    /// The current choice's display name.
    pub fn current_name(&self) -> String {
        self.choices
            .iter()
            .find(|c| c.value == self.current)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| self.current.clone())
    }
}

/// A command an agent offers, invoked by sending `/name`.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCommand {
    pub name: String,
    pub description: String,
}
