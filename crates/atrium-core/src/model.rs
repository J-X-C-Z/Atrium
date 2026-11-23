use serde::{Deserialize, Serialize};

/// The id of a request the backend made to us (such as an approval), echoed
/// back verbatim in the answer. JSON-RPC allows numbers or strings.
pub type RequestId = serde_json::Value;

/// A conversation as listed in the sidebar.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChangeKind {
    Add,
    Delete,
    Update { move_to: Option<String> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Tokens of one kind of use, as far as the backend breaks them down.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TokenCounts {
    pub total: u64,
    /// Prompt tokens, cached ones included.
    pub input: u64,
    /// The part of `input` served from the prompt cache.
    pub cached_input: u64,
    /// Tokens the model wrote, reasoning included.
    pub output: u64,
    /// The part of `output` spent reasoning.
    pub reasoning: u64,
}

impl TokenCounts {
    /// Whether the backend said more than a total.
    pub fn has_breakdown(&self) -> bool {
        self.input > 0 || self.output > 0
    }

    pub fn add(&mut self, other: &TokenCounts) {
        self.total += other.total;
        self.input += other.input;
        self.cached_input += other.cached_input;
        self.output += other.output;
        self.reasoning += other.reasoning;
    }
}

/// What a conversation has used, and how full its context is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TokenUsage {
    /// The whole conversation so far.
    pub total: TokenCounts,
    /// The most recent model request (or turn, for agents that report per turn).
    pub last: TokenCounts,
    /// Tokens in the context window now.
    pub context_used: Option<u64>,
    pub context_window: Option<u64>,
    /// What the conversation cost, for agents billed per token.
    pub cost: Option<Cost>,
}

impl TokenUsage {
    /// Takes in a newer report. Agents may report context and counts in
    /// separate messages, in either order, so what a report leaves out keeps
    /// its last known value.
    pub fn merge(&mut self, newer: &TokenUsage) {
        if newer.total.total > 0 || newer.total.has_breakdown() {
            self.total = newer.total;
        }
        if newer.last.total > 0 || newer.last.has_breakdown() {
            self.last = newer.last;
        }
        if newer.context_used.is_some() {
            self.context_used = newer.context_used;
        }
        if newer.context_window.is_some() {
            self.context_window = newer.context_window;
        }
        if newer.cost.is_some() {
            self.cost.clone_from(&newer.cost);
        }
    }

    /// How full the context is, 0 to 1, when the window is known.
    pub fn context_share(&self) -> Option<f64> {
        let window = self.context_window.filter(|w| *w > 0)?;
        Some((self.context_used? as f64 / window as f64).clamp(0., 1.))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cost {
    pub amount: f64,
    pub currency: String,
}

/// One thing that happened in a turn, normalized across backends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
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
    /// The current choice's display name. When `current` is not among the
    /// choices as written, it is matched ignoring case, then by the part
    /// after the last `/`; failing that, the id is made readable.
    pub fn current_name(&self) -> String {
        if let Some(choice) = self.current_choice() {
            return choice.name.clone();
        }
        static LOGGED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        if let Ok(mut logged) = LOGGED.lock()
            && !logged.contains(&self.current)
        {
            log::debug!(
                "option {} is {:?}, which is not among its {} choices",
                self.id,
                self.current,
                self.choices.len()
            );
            logged.push(self.current.clone());
        }
        prettify_model_id(&self.current)
    }

    /// The choice `current` names, allowing for case and a `group/` prefix.
    fn current_choice(&self) -> Option<&ConfigChoice> {
        let current = self.current.as_str();
        let tail = |v: &str| v.rsplit('/').next().unwrap_or(v).to_lowercase();
        self.choices
            .iter()
            .find(|c| c.value == current)
            .or_else(|| {
                self.choices
                    .iter()
                    .find(|c| c.value.eq_ignore_ascii_case(current))
            })
            .or_else(|| {
                let want = tail(current);
                self.choices.iter().find(|c| tail(&c.value) == want)
            })
    }
}

/// A model id made readable: `group/auto-deepseek-v4-1-flash` becomes
/// "Auto DeepSeek V4.1 Flash".
pub fn prettify_model_id(id: &str) -> String {
    let id = id.trim();
    let tail = id.rsplit('/').next().unwrap_or(id);
    let tokens: Vec<&str> = tail
        .split(['-', '_', ' '])
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return id.to_string();
    }
    let version_part =
        |t: &str| !t.is_empty() && t.len() <= 2 && t.bytes().all(|b| b.is_ascii_digit());
    let mut words: Vec<String> = Vec::new();
    let mut i = 0;
    while i < tokens.len() {
        let token = tokens[i];
        i += 1;
        // A number, or `v` and a number, followed by small numbers is one
        // dotted version: `v4-1` -> `V4.1`.
        let number = token.strip_prefix(['v', 'V']).unwrap_or(token);
        if version_part(number) && tokens.get(i).is_some_and(|t| version_part(t)) {
            let mut version = capitalise_token(token);
            while let Some(next) = tokens.get(i).filter(|t| version_part(t)) {
                version.push('.');
                version.push_str(next);
                i += 1;
            }
            words.push(version);
            continue;
        }
        words.push(capitalise_token(token));
    }
    words.join(" ")
}

fn capitalise_token(token: &str) -> String {
    match token.to_lowercase().as_str() {
        "deepseek" => return "DeepSeek".into(),
        "gpt" => return "GPT".into(),
        "claude" => return "Claude".into(),
        "gemini" => return "Gemini".into(),
        "qwen" => return "Qwen".into(),
        "glm" => return "GLM".into(),
        _ => {}
    }
    let mut chars = token.chars();
    match chars.next() {
        // Words with digits keep the rest as written (`4o`, `r1`).
        Some(c) if token.chars().any(|c| c.is_ascii_digit()) => {
            c.to_uppercase().chain(chars).collect()
        }
        Some(c) => c
            .to_uppercase()
            .chain(chars.flat_map(|c| c.to_lowercase()))
            .collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn option(current: &str, values: &[(&str, &str)]) -> ConfigOption {
        ConfigOption {
            id: "model".into(),
            name: "Model".into(),
            category: Some("model".into()),
            current: current.into(),
            choices: values
                .iter()
                .map(|(value, name)| ConfigChoice {
                    value: (*value).into(),
                    name: (*name).into(),
                    description: None,
                    group: None,
                })
                .collect(),
        }
    }

    #[test]
    fn current_name_matches_exactly_then_loosely() {
        let o = option("a", &[("a", "Alpha")]);
        assert_eq!(o.current_name(), "Alpha");
        let o = option("GROUP/Auto-Flash", &[("group/auto-flash", "Auto Flash")]);
        assert_eq!(o.current_name(), "Auto Flash");
        let o = option("group/auto-flash", &[("other/auto-flash", "Auto Flash")]);
        assert_eq!(o.current_name(), "Auto Flash");
    }

    #[test]
    fn unmatched_ids_are_prettified() {
        let o = option("group/auto-deepseek-v4-1-flash", &[("x", "X")]);
        assert_eq!(o.current_name(), "Auto DeepSeek V4.1 Flash");
        assert_eq!(prettify_model_id("gpt-4o-mini"), "GPT 4o Mini");
        assert_eq!(prettify_model_id("claude-3-5-sonnet"), "Claude 3.5 Sonnet");
        assert_eq!(prettify_model_id("qwen3_coder"), "Qwen3 Coder");
        assert_eq!(prettify_model_id("glm-4-6"), "GLM 4.6");
        assert_eq!(
            prettify_model_id("claude-sonnet-4-20250514"),
            "Claude Sonnet 4 20250514"
        );
        assert_eq!(prettify_model_id("gemini-2-5-pro"), "Gemini 2.5 Pro");
        assert_eq!(prettify_model_id(""), "");
        assert_eq!(prettify_model_id("a/"), "a/");
    }
}

/// A command an agent offers, invoked by sending `/name`.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCommand {
    pub name: String,
    pub description: String,
}

/// An MCP server a session should have: a command run over stdio, or, when
/// `url` is set, a Streamable HTTP server the session connects to (`command`,
/// `args` and `env` are then unused). The `atrium` tools reach agents on
/// other computers this way.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct McpServer {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// The server's address, for an HTTP server.
    pub url: Option<String>,
    /// Headers sent with every request, for an HTTP server.
    pub headers: Vec<(String, String)>,
}

impl McpServer {
    /// A Streamable HTTP server.
    pub fn http(name: &str, url: &str, headers: Vec<(String, String)>) -> Self {
        Self {
            name: name.into(),
            url: Some(url.into()),
            headers,
            ..Self::default()
        }
    }
}
