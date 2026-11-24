// Ported from apps/server/src/provider/Drivers/AntigravitySkills.ts,
// apps/server/src/provider/Drivers/antigravityUsageReader.ts,
// apps/server/src/provider/Drivers/AntigravityUsage.ts,
// apps/server/src/provider/antigravityAuthSupport.ts,
// apps/server/src/provider/acp/AntigravityProtocol.ts and
// apps/server/src/provider/acp/AntigravityChildWork.ts (T3 Code v0.1.11, MIT).
// The Effect services, YAML parser and Node SQLite client are replaced by
// std, a small frontmatter reader and rusqlite; the paths, limits, field
// numbers, model normalization and record shape are kept.
//!
//! What Antigravity does beyond plain ACP: method-based sign-in, skills read
//! from its profile, a usage reader over its local conversation databases
//! (typed here; item 26 owns the report), and child-work launches carried on
//! `invoke_subagent` tool calls.

use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use base64::Engine as _;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::{Value, json};

use super::adapter::SkillGroup;

/// The four sign-in methods the installed `agy` advertises.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AntigravityAuthMethod {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
}

impl AntigravityAuthMethod {
    pub const ALL: &'static [Self] = &[
        Self {
            id: "oauth-personal",
            name: "Log in with Google",
            description: "Log in with your Google account",
        },
        Self {
            id: "oauth-business",
            name: "Log in with Gemini Enterprise",
            description: "Log in with your Gemini Enterprise account",
        },
        Self {
            id: "gemini-api-key",
            name: "Gemini API key",
            description: "Use an API key with Gemini Developer API",
        },
        Self {
            id: "agent-platform",
            name: "Gemini Enterprise Agent Platform",
            description: "Use Gemini Enterprise Agent Platform with Application Default Credentials or an API key",
        },
    ];
}

/// T3's `antigravityAuthLabel`.
pub fn auth_label(method: &str) -> &'static str {
    match method {
        "oauth-personal" => "Google account",
        "oauth-business" => "Gemini Enterprise",
        "gemini-api-key" => "Gemini API key",
        "agent-platform" => "Agent Platform",
        _ => "Antigravity",
    }
}

/// T3's `antigravityAuthUsesBrowser`: the two OAuth methods open a browser.
pub fn auth_uses_browser(method: &str) -> bool {
    matches!(method, "oauth-personal" | "oauth-business")
}

/// The settings an instance must supply for the selected method.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AntigravityAuthConfig {
    pub auth_method: String,
    pub api_key: String,
    pub gcp_project: String,
    pub gcp_location: String,
}

/// T3's `antigravityAuthConfigIssue`; `None` means the selection is usable.
pub fn auth_config_issue(auth: &AntigravityAuthConfig) -> Option<&'static str> {
    match auth.auth_method.as_str() {
        "oauth-personal" | "" => None,
        "oauth-business" => {
            if auth.gcp_project.is_empty() || auth.gcp_location.is_empty() {
                Some(
                    "Gemini Enterprise needs a GCP project and location in the Antigravity provider settings.",
                )
            } else {
                None
            }
        }
        "gemini-api-key" => {
            if auth.api_key.is_empty() {
                Some("Enter a Gemini API key in the Antigravity provider settings.")
            } else {
                None
            }
        }
        "agent-platform" => {
            if auth.api_key.is_empty()
                && (auth.gcp_project.is_empty() || auth.gcp_location.is_empty())
            {
                Some(
                    "Agent Platform needs an API key, or a GCP project and location, in the Antigravity provider settings.",
                )
            } else {
                None
            }
        }
        _ => Some("Choose a sign-in method in the Antigravity provider settings."),
    }
}

// ---------------------------------------------------------------------------
// model names

/// T3's `modelName`: "Gemini 9.1 Flash (High)" reads as
/// `gemini-9.1-flash-high`; Claude names move the version after the family.
pub fn model_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let mut normalized = name.to_ascii_lowercase();
    if let Some(start) = normalized.rfind(" (")
        && normalized.ends_with(')')
    {
        let inner = normalized[start + 2..normalized.len() - 1].to_string();
        normalized = format!("{}-{inner}", &normalized[..start]);
    }
    normalized = normalized.split_whitespace().collect::<Vec<_>>().join("-");
    if let Some(rest) = normalized.strip_prefix("claude-") {
        let (version, after_version) = rest.split_once('-').unwrap_or((rest, ""));
        let (family, suffix) = after_version.split_once('-').unwrap_or((after_version, ""));
        if matches!(family, "sonnet" | "opus" | "haiku")
            && version
                .chars()
                .all(|character| character.is_ascii_digit() || character == '.')
        {
            // T3 moves the version after the family and then replaces every
            // dot in the result, so a `-thinking` suffix stays last.
            let rebuilt = if suffix.is_empty() {
                format!("claude-{family}-{version}")
            } else {
                format!("claude-{family}-{version}-{suffix}")
            };
            normalized = rebuilt.replace('.', "-");
        }
    }
    normalized
}

/// T3's `isConcreteModel`.
pub fn is_concrete_model(model: &str) -> bool {
    let concrete = model.starts_with("gemini-")
        && model
            .as_bytes()
            .get("gemini-".len())
            .is_some_and(u8::is_ascii_digit)
        || model.starts_with("claude-sonnet-")
        || model.starts_with("claude-opus-")
        || model.starts_with("claude-haiku-")
        || model.starts_with("gpt-");
    let alias = model.starts_with("gemini-3-flash-agent")
        || matches!(model, "gemini-3-flash-a" | "gemini-3-flash-b");
    concrete && !alias
}

/// T3's `antigravityRateModel`: effort and routing variants share the base
/// model's price.
pub fn rate_model(model: &str) -> String {
    let base = model
        .strip_suffix("-extra-low")
        .or_else(|| model.strip_suffix("-low"))
        .or_else(|| model.strip_suffix("-medium"))
        .or_else(|| model.strip_suffix("-high"))
        .or_else(|| model.strip_suffix("-tiered"))
        .or_else(|| model.strip_suffix("-control"))
        .or_else(|| model.strip_suffix("-thinking"))
        .or_else(|| model.strip_suffix("-agent"))
        .unwrap_or(model);
    if model.starts_with("gemini-3-flash-a") || model.starts_with("gemini-3-flash-b") {
        return "gemini-3.5-flash".into();
    }
    if model == "gemini-3.7-flash-safety-le" {
        return "gemini-3.7-flash".into();
    }
    if base == "gemini-3-flash" {
        return "gemini-3-flash-preview".into();
    }
    base.to_string()
}

// ---------------------------------------------------------------------------
// skills

/// T3's `resolveAntigravityUserHome`.
pub fn resolve_user_home(environment_home: Option<&str>, fallback: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            return PathBuf::from(profile);
        }
        if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH")) {
            return PathBuf::from(format!("{drive}{path}"));
        }
    }
    environment_home
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback.to_path_buf())
}

/// T3's `antigravityUserSkillDirectories`: both live under the profile's
/// linked `~/.gemini`, not the project's `.agents/skills` directory.
pub fn user_skill_directories(gemini_home: &Path) -> (PathBuf, PathBuf) {
    (
        gemini_home.join("config").join("skills"),
        gemini_home.join("antigravity-cli").join("skills"),
    )
}

const MAX_SKILL_BYTES: u64 = 1_000_000;
const MAX_SCAN_BYTES: u64 = 8_000_000;
const MAX_SCAN_ENTRIES: usize = 10_000;

struct Scanner {
    budget_bytes: u64,
    budget_entries: usize,
    skills: HashMap<String, Value>,
    errors: Vec<Value>,
}

impl Scanner {
    fn scan_dir(&mut self, directory: &Path, scope: &str, children: bool) {
        if self.budget_bytes == 0 || self.budget_entries == 0 {
            self.errors.push(json!({
                "reason": "scan-budget-exhausted",
                "path": directory.display().to_string(),
            }));
            return;
        }
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        let mut names: Vec<String> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect();
        if names.len() > self.budget_entries {
            self.errors.push(json!({
                "reason": "scan-budget-exhausted",
                "path": directory.display().to_string(),
            }));
            return;
        }
        self.budget_entries -= names.len();
        names.sort();
        if let Some(skill_file) = names
            .iter()
            .find(|name| name.eq_ignore_ascii_case("skill.md"))
        {
            if !skill_file.to_ascii_lowercase().ends_with(".md") {
                return;
            }
            let path = directory.join(skill_file);
            let Ok(contents) = read_bounded(&path, self.budget_bytes) else {
                return;
            };
            let (contents, bytes) = contents;
            self.budget_bytes = self.budget_bytes.saturating_sub(bytes);
            let Some((name, description)) = parse_skill_frontmatter(&contents, skill_file) else {
                return;
            };
            self.skills.entry(name.clone()).or_insert_with(|| {
                json!({
                    "name": name,
                    "description": description,
                    "path": path.display().to_string(),
                    "scope": scope,
                    "enabled": true,
                })
            });
            return;
        }
        if children {
            // The native loader orders child paths with Go's escaped-path encoding.
            names.sort_by_key(|name| skill_path_sort_key(name));
            for name in names {
                self.scan_dir(&directory.join(name), scope, false);
            }
        }
    }
}

/// Reads at most `budget + 1` bytes and reports how many were read, as T3's
/// bounded frontmatter read does.
fn read_bounded(path: &Path, budget: u64) -> std::io::Result<(String, u64)> {
    let contents = fs::read(path)?;
    let limit = budget.min(MAX_SKILL_BYTES) as usize;
    let slice = &contents[..contents.len().min(limit + 1)];
    Ok((
        String::from_utf8_lossy(slice).into_owned(),
        slice.len() as u64,
    ))
}

fn skill_path_sort_key(entry: &str) -> String {
    // Go's URL.EscapedPath escapes the characters `encodeURI` leaves alone.
    entry
        .chars()
        .map(|character| match character {
            '!' | '\'' | '(' | ')' | '*' | '?' | '#' => {
                format!("%{:X}", character as u32)
            }
            other => other.to_string(),
        })
        .collect()
}

/// Reads `name` and `description` from the two `---` markers. T3 parses the
/// whole document as YAML; the fields it reads are scalar keys.
fn parse_skill_frontmatter(contents: &str, file_name: &str) -> Option<(String, Option<String>)> {
    let start = contents.find("---")?;
    let end = contents[start + 3..].find("---")? + start + 3;
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    for line in contents[start + 3..end].lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'');
        match key.trim() {
            "name" if !value.is_empty() => name = Some(value.to_string()),
            "description" if !value.is_empty() => description = Some(value.to_string()),
            _ => {}
        }
    }
    let name = name.unwrap_or_else(|| {
        file_name
            .strip_suffix(".md")
            .unwrap_or(file_name)
            .to_string()
    });
    if name.is_empty() || name != name.trim() {
        return None;
    }
    Some((name, description))
}

/// Discovers the skills one folder offers, in T3's order. `profile_directory`
/// is the instance's `GEMINI_HOME`, when one is managed.
pub fn discover_skills(
    cwd: &str,
    user_home: &Path,
    profile_directory: Option<&Path>,
) -> SkillGroup {
    let cwd = Path::new(cwd);
    let gemini_home = user_home.join(".gemini");
    let (config_skills, cli_skills) = user_skill_directories(&gemini_home);
    let roots = [
        (config_skills, "user"),
        (cwd.join(".gemini").join("skills"), "project"),
        (cli_skills, "user"),
        (cwd.join(".agents").join("skills"), "project"),
        (cwd.join(".agent").join("skills"), "project"),
    ];
    let mut scanner = Scanner {
        budget_bytes: MAX_SCAN_BYTES,
        budget_entries: MAX_SCAN_ENTRIES,
        skills: HashMap::new(),
        errors: Vec::new(),
    };
    for (root, scope) in roots {
        scanner.scan_dir(&root, scope, true);
    }
    if let Some(profile) = profile_directory {
        let plugins_directory = profile.join("config").join("plugins");
        if let Ok(plugins) = fs::read_dir(&plugins_directory) {
            let disabled = disabled_plugins(&profile.join("config").join("config.json"));
            let mut names: Vec<String> = plugins
                .filter_map(|entry| entry.ok())
                .filter_map(|entry| entry.file_name().into_string().ok())
                .collect();
            names.sort();
            for plugin in names {
                if disabled.contains(&plugin) {
                    continue;
                }
                let dir = plugins_directory.join(plugin);
                if dir.is_dir() {
                    scanner.scan_dir(&dir.join("skills"), "user", true);
                }
            }
        }
    }
    let mut skills: Vec<Value> = scanner.skills.into_values().collect();
    skills.sort_by_key(|skill| {
        skill["name"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase()
    });
    SkillGroup {
        cwd: cwd.display().to_string(),
        skills,
        errors: scanner.errors,
    }
}

fn disabled_plugins(config_path: &Path) -> HashSet<String> {
    let Ok(contents) = fs::read_to_string(config_path) else {
        return HashSet::new();
    };
    let Ok(config) = serde_json::from_str::<Value>(&contents) else {
        return HashSet::new();
    };
    config["plugins"]
        .as_object()
        .map(|plugins| {
            plugins
                .iter()
                .filter(|(_, value)| value["enabled"] == Value::Bool(false))
                .map(|(name, _)| name.clone())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// child work

/// T3's `ANTIGRAVITY_SUBAGENT_BATCH_TITLE`.
pub const SUBAGENT_BATCH_TITLE: &str = "Antigravity subagent batch";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentLaunch {
    pub role: String,
    pub prompt: String,
}

/// T3's `antigravitySubagentLaunches`: agy 1.5 sends one flat
/// `{Role, Prompt}`; earlier releases send `{Subagents: [...]}`.
pub fn subagent_launches(input: &Value) -> Option<Vec<SubagentLaunch>> {
    let one = |value: &Value| -> Option<SubagentLaunch> {
        Some(SubagentLaunch {
            role: value["Role"].as_str()?.to_string(),
            prompt: value["Prompt"].as_str()?.to_string(),
        })
    };
    if let Some(children) = input["Subagents"].as_array() {
        if children.is_empty() {
            return None;
        }
        return children.iter().map(one).collect();
    }
    one(input).map(|launch| vec![launch])
}

/// T3's `describeAntigravitySubagentLaunches`: one child keeps its own role
/// and prompt; a batch joins roles and prompts.
pub fn describe_subagent_launches(launches: &[SubagentLaunch]) -> (String, String) {
    if let [only] = launches {
        return (only.role.clone(), only.prompt.clone());
    }
    (
        launches
            .iter()
            .map(|launch| launch.role.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        launches
            .iter()
            .map(|launch| format!("{}\n{}", launch.role, launch.prompt))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
}

/// T3 bounds tool text to its tail; `invoke_subagent` launch text uses the
/// same bound.
pub fn bound_tool_text(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        text.to_string()
    } else {
        format!(
            "[Earlier output truncated]\n\n{}",
            &text[text.len() - limit..]
        )
    }
}

// ---------------------------------------------------------------------------
// usage reader

/// One usage sample's counters, in T3's `UsageTokenTotals` shape.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

impl UsageTotals {
    fn is_zero(&self) -> bool {
        self.uncached_input_tokens
            + self.cached_input_tokens
            + self.cache_creation_tokens
            + self.output_tokens
            == 0
    }

    fn merge_max(&mut self, other: &Self) {
        self.uncached_input_tokens = self.uncached_input_tokens.max(other.uncached_input_tokens);
        self.cached_input_tokens = self.cached_input_tokens.max(other.cached_input_tokens);
        self.cache_creation_tokens = self.cache_creation_tokens.max(other.cache_creation_tokens);
        self.output_tokens = self.output_tokens.max(other.output_tokens);
        self.reasoning_tokens = self.reasoning_tokens.max(other.reasoning_tokens);
    }
}

/// One record in the shape item 26 reports (T3's `UsageRecord`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageRecord {
    pub provider: &'static str,
    pub session_id: String,
    pub timestamp_ms: i64,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_model: Option<String>,
    pub totals: UsageTotals,
    pub reported_cost_usd: Option<f64>,
    pub speed: &'static str,
    pub dedupe_key: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageFile {
    pub root: String,
    pub path: String,
    pub records: Vec<UsageRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageScan {
    pub files: Vec<UsageFile>,
    pub errors: Vec<String>,
}

/// The offline names T3 keeps for records that carry only a numeric id.
/// Cross-checked against ccusage's Antigravity parser; this port keeps the
/// ids the pinned release documents. An id outside the table falls back to
/// the learned name, then `antigravity-model-<id>`.
const MODEL_IDS: &[(u64, &str)] = &[
    (246, "gemini-2.5-pro"),
    (312, "gemini-2.5-flash"),
    (313, "gemini-2.5-flash-thinking"),
    (329, "gemini-2.5-flash-thinking"),
    (330, "gemini-2.5-flash-lite"),
    (281, "claude-sonnet-4"),
    (282, "claude-sonnet-4"),
    (290, "claude-opus-4"),
    (291, "claude-opus-4"),
    (333, "claude-sonnet-4-5"),
    (334, "claude-sonnet-4-5"),
    (340, "claude-haiku-4-5"),
    (341, "claude-haiku-4-5"),
    (342, "gpt-oss-120b-medium"),
    (1016, "gemini-3.1-pro"),
    (1035, "claude-sonnet-4-6"),
    (1036, "gemini-3.1-pro"),
    (1037, "gemini-3.1-pro"),
    (1018, "gemini-3-flash-preview"),
    (1084, "gemini-3-flash-preview"),
    (1047, "gemini-3-flash-preview"),
    (1020, "gemini-3.5-flash-medium"),
    (1021, "gemini-3.1-flash-image"),
    (1050, "gemini-3.1-flash-lite"),
];

fn offline_model(id: u64) -> Option<&'static str> {
    MODEL_IDS
        .iter()
        .find_map(|(candidate, name)| (*candidate == id).then_some(*name))
}

/// T3's `readAntigravityUsage`, typed. Scans each directory recursively for
/// `.db` conversation stores and returns records newer than `since_ms`.
#[derive(Clone, Debug, Default)]
pub struct AntigravityUsageReader {
    directory: Option<PathBuf>,
}

impl AntigravityUsageReader {
    /// `directory` is the instance profile (or a data root) to scan when the
    /// caller does not name roots explicitly.
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self { directory }
    }

    pub fn default_roots(home: &Path) -> Vec<PathBuf> {
        [
            ".gemini/antigravity",
            ".gemini/antigravity-cli",
            ".gemini/antigravity-ide",
            ".gemini/antigravity-backup",
            ".config/antigravity",
        ]
        .iter()
        .map(|relative| home.join(relative))
        .filter(|path| path.is_dir())
        .collect()
    }

    /// Scans this reader's directory, or the Antigravity data roots under
    /// `home` when none was configured.
    pub fn scan_home(&self, home: &Path, since_ms: i64) -> UsageScan {
        let roots: Vec<PathBuf> = self
            .directory
            .clone()
            .map(|directory| vec![directory])
            .unwrap_or_else(|| Self::default_roots(home));
        self.scan(&roots, since_ms)
    }

    pub fn scan(&self, roots: &[PathBuf], since_ms: i64) -> UsageScan {
        let mut scan = UsageScan::default();
        let mut databases: Vec<(PathBuf, PathBuf, Option<Database>)> = Vec::new();
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for root in roots {
            walk(root, root, &mut scan.errors, &mut seen, &mut databases);
        }
        // Learn numeric ids from every file before resolving any record, as
        // T3 does so evidence in a later-named database still names an id.
        let mut models: HashMap<u64, Option<String>> = HashMap::new();
        for (_, _, database) in &databases {
            if let Some(database) = database {
                for (id, model) in &database.models {
                    remember_model(&mut models, *id, model.as_deref());
                }
            }
        }
        let mut groups: Vec<Candidate> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();
        for (root, path, database) in databases {
            let Some(database) = database else {
                continue;
            };
            let session_id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_string();
            let file_index = scan.files.len();
            scan.files.push(UsageFile {
                root: root.display().to_string(),
                path: path.display().to_string(),
                records: Vec::new(),
            });
            for candidate in usage_candidates(&session_id, database, &models) {
                let key = candidate.record.dedupe_key.clone();
                if let Some(&existing) = by_key.get(&key) {
                    groups[existing].merge(candidate);
                    continue;
                }
                by_key.insert(key, groups.len());
                groups.push(Candidate {
                    record: candidate.record,
                    file_index,
                });
            }
        }
        for group in groups {
            if group.record.timestamp_ms >= since_ms
                && let Some(file) = scan.files.get_mut(group.file_index)
            {
                file.records.push(group.record);
            }
        }
        scan
    }
}

struct Candidate {
    record: UsageRecord,
    file_index: usize,
}

impl Candidate {
    fn merge(&mut self, other: Self) {
        self.record.totals.merge_max(&other.record.totals);
        if self.record.timestamp_ms == 0 && other.record.timestamp_ms != 0 {
            self.record.timestamp_ms = other.record.timestamp_ms;
        }
        if !is_concrete_model(&self.record.model) && is_concrete_model(&other.record.model) {
            self.record.model = other.record.model;
            self.record.rate_model = other.record.rate_model;
        }
    }
}

fn remember_model(models: &mut HashMap<u64, Option<String>>, id: u64, model: Option<&str>) {
    let model = model.map(str::to_owned);
    match models.get_mut(&id) {
        None => {
            models.insert(id, model);
        }
        Some(previous) if previous.as_deref() == model.as_deref() => {}
        Some(previous) => {
            *previous = None;
        }
    }
}

fn walk(
    directory: &Path,
    root: &Path,
    errors: &mut Vec<String>,
    seen: &mut HashSet<PathBuf>,
    databases: &mut Vec<(PathBuf, PathBuf, Option<Database>)>,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        errors.push(directory.display().to_string());
        return;
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect();
    names.sort();
    for name in names {
        let path = directory.join(&name);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            walk(&path, root, errors, seen, databases);
        } else if name.ends_with(".db") {
            let canonical = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if !seen.insert(canonical) {
                continue;
            }
            let database = match read_database(&path) {
                Ok(database) => database,
                Err(_) => {
                    errors.push(path.display().to_string());
                    databases.push((root.to_path_buf(), path.clone(), None));
                    continue;
                }
            };
            databases.push((root.to_path_buf(), path, database));
        }
    }
}

struct Database {
    generation: Vec<(i64, Metadata)>,
    steps: Vec<(i64, Metadata)>,
    trajectory_timestamp: Option<i64>,
    models: HashMap<u64, Option<String>>,
}

#[derive(Clone, Debug, Default)]
struct Metadata {
    model: String,
    model_id: u64,
    timestamp_ms: Option<i64>,
    usages: Vec<Vec<u8>>,
}

fn read_database(path: &Path) -> Result<Option<Database>, String> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| error.to_string())?;
    let _ = connection.busy_timeout(Duration::from_millis(100));
    let tables: HashSet<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|error| error.to_string())?
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| error.to_string())?
        .filter_map(Result::ok)
        .collect();
    if !tables.contains("gen_metadata") && !tables.contains("steps") {
        return Ok(None);
    }
    let generation = read_metadata(&connection, "gen_metadata", "data", false)?;
    let steps = read_metadata(&connection, "steps", "metadata", true)?;
    let mut trajectory_timestamp = None;
    if tables.contains("trajectory_metadata_blob") {
        let mut statement = connection
            .prepare("SELECT data FROM trajectory_metadata_blob")
            .map_err(|error| error.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(|error| error.to_string())?;
        for row in rows.flatten() {
            if trajectory_timestamp.is_none()
                && let Ok(fields) = parse_fields(&row)
                && let Some(nested) = nested(&fields, 2)
            {
                trajectory_timestamp = timestamp(&nested);
            }
        }
    }
    let mut models = HashMap::new();
    for (_, entry) in generation.iter().chain(steps.iter()) {
        if entry.model_id > 0 && is_concrete_model(&entry.model) {
            remember_model(&mut models, entry.model_id, Some(&entry.model));
        }
    }
    Ok(Some(Database {
        generation,
        steps,
        trajectory_timestamp,
        models,
    }))
}

fn read_metadata(
    connection: &Connection,
    table: &str,
    column: &str,
    step: bool,
) -> Result<Vec<(i64, Metadata)>, String> {
    let sql = format!("SELECT idx, {column} FROM {table} ORDER BY idx");
    let mut statement = connection
        .prepare(&sql)
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<Vec<u8>>>(1)?))
        })
        .map_err(|error| error.to_string())?;
    let mut entries = Vec::new();
    for row in rows.flatten() {
        let (index, data) = row;
        if step && data.is_none() {
            continue;
        }
        let Some(data) = data else { continue };
        if let Ok(fields) = parse_fields(&data)
            && let Some(entry) = metadata(&fields, step)
        {
            entries.push((index, entry));
        }
    }
    entries.sort_by_key(|(index, _)| *index);
    Ok(entries)
}

// A minimal protobuf reader: length-delimited values are kept, varints read.
#[derive(Clone, Debug)]
enum Field {
    Varint(u64),
    Bytes(Vec<u8>),
}

type Fields = HashMap<u64, Vec<Field>>;

fn parse_fields(bytes: &[u8]) -> Result<Fields, String> {
    let mut fields: Fields = HashMap::new();
    let mut offset = 0;
    while offset < bytes.len() {
        let tag = varint(bytes, &mut offset)?;
        let number = tag / 8;
        let wire = tag % 8;
        if number == 0 {
            return Err("invalid protobuf field".into());
        }
        let value = match wire {
            0 => Field::Varint(varint(bytes, &mut offset)?),
            1 => {
                if offset + 8 > bytes.len() {
                    return Err("truncated protobuf field".into());
                }
                offset += 8;
                continue;
            }
            2 => {
                let length = varint(bytes, &mut offset)? as usize;
                if length > bytes.len().saturating_sub(offset) {
                    return Err("truncated protobuf field".into());
                }
                let value = bytes[offset..offset + length].to_vec();
                offset += length;
                Field::Bytes(value)
            }
            5 => {
                if offset + 4 > bytes.len() {
                    return Err("truncated protobuf field".into());
                }
                offset += 4;
                continue;
            }
            _ => return Err("unsupported protobuf wire type".into()),
        };
        fields.entry(number).or_default().push(value);
    }
    Ok(fields)
}

fn varint(bytes: &[u8], offset: &mut usize) -> Result<u64, String> {
    let mut value = 0u64;
    for shift in 0..10 {
        let byte = *bytes.get(*offset).ok_or("truncated varint")?;
        *offset += 1;
        value |= u64::from(byte & 0x7f) << (shift * 7);
        if byte < 0x80 {
            return Ok(value);
        }
    }
    Err("overlong varint".into())
}

fn number_at(fields: &Fields, key: u64) -> u64 {
    match fields.get(&key).and_then(|values| values.first()) {
        Some(Field::Varint(value)) => *value,
        _ => 0,
    }
}

fn bytes_at(fields: &Fields, key: u64) -> Option<Vec<u8>> {
    match fields.get(&key).and_then(|values| values.first()) {
        Some(Field::Bytes(value)) => Some(value.clone()),
        _ => None,
    }
}

fn nested(fields: &Fields, key: u64) -> Option<Fields> {
    parse_fields(&bytes_at(fields, key)?).ok()
}

fn text_at(fields: &Fields, key: u64) -> String {
    bytes_at(fields, key)
        .map(|bytes| String::from_utf8_lossy(&bytes).trim().to_string())
        .unwrap_or_default()
}

fn timestamp(fields: &Fields) -> Option<i64> {
    let seconds = number_at(fields, 1);
    (seconds > 0).then_some((seconds * 1000 + number_at(fields, 2) / 1_000_000) as i64)
}

fn metadata(root: &Fields, step: bool) -> Option<Metadata> {
    let data = if step { root.clone() } else { nested(root, 1)? };
    let model = if step {
        nested(&data, 24).unwrap_or_default()
    } else {
        data.clone()
    };
    let usage = bytes_at(&data, if step { 9 } else { 4 });
    let primary = usage.as_deref().and_then(|usage| parse_fields(usage).ok());
    let mut usages: Vec<Vec<u8>> = primary
        .is_some()
        .then(|| usage.unwrap())
        .into_iter()
        .collect();
    for retry in data
        .get(&(if step { 28 } else { 17 }))
        .cloned()
        .unwrap_or_default()
    {
        if let Field::Bytes(retry) = retry
            && let Ok(retry) = parse_fields(&retry)
            && let Some(usage) = bytes_at(&retry, 2)
        {
            usages.push(usage);
        }
    }
    let names = [
        model_name(&text_at(&model, if step { 12 } else { 19 })),
        model_name(&text_at(&model, if step { 8 } else { 21 })),
    ];
    let model_text = names
        .iter()
        .find(|name| is_concrete_model(name))
        .or_else(|| {
            names.iter().find(|name| {
                !name.is_empty() && !name.starts_with("model_") && !name.starts_with("model-")
            })
        })
        .cloned()
        .unwrap_or_default();
    let model_id = number_at(&model, if step { 1 } else { 3 })
        .max(primary.as_ref().map_or(0, |usage| number_at(usage, 1)));
    let timestamp_ms = if step {
        nested(&data, 8)
            .and_then(|value| timestamp(&value))
            .or_else(|| nested(&data, 1).and_then(|value| timestamp(&value)))
    } else {
        nested(&data, 9)
            .and_then(|value| nested(&value, 4))
            .and_then(|value| timestamp(&value))
    };
    Some(Metadata {
        model: model_text,
        model_id,
        timestamp_ms,
        usages,
    })
}

fn usage_candidates(
    session_id: &str,
    database: Database,
    models: &HashMap<u64, Option<String>>,
) -> Vec<Candidate> {
    let generation_models: HashMap<i64, &Metadata> = database
        .generation
        .iter()
        .map(|(index, entry)| (*index, entry))
        .collect();
    let mut records = Vec::new();
    for (source, entries) in [
        ("step", &database.steps),
        ("generation", &database.generation),
    ] {
        for (index, (row_index, entry)) in entries.iter().enumerate() {
            for (usage_index, usage) in entry.usages.iter().enumerate() {
                let Ok(fields) = parse_fields(usage) else {
                    continue;
                };
                let usage_model_id = number_at(&fields, 1);
                let model_id = if usage_model_id > 0 {
                    usage_model_id
                } else {
                    entry.model_id
                };
                // A retry can use a different model than its enclosing step's generation.
                let generation = (source == "step")
                    .then(|| generation_models.get(row_index))
                    .flatten()
                    .copied();
                let matches = |metadata: &Metadata| model_id == 0 || metadata.model_id == model_id;
                let model = if matches(entry) && !entry.model.is_empty() {
                    entry.model.clone()
                } else if let Some(generation) = generation.filter(|generation| matches(generation))
                {
                    generation.model.clone()
                } else {
                    String::new()
                };
                let output_tokens =
                    number_at(&fields, 3).max(number_at(&fields, 9) + number_at(&fields, 10));
                let totals = UsageTotals {
                    uncached_input_tokens: number_at(&fields, 2),
                    cached_input_tokens: number_at(&fields, 5),
                    cache_creation_tokens: number_at(&fields, 4),
                    output_tokens,
                    reasoning_tokens: output_tokens.min(number_at(&fields, 9)),
                };
                if totals.is_zero() {
                    continue;
                }
                let keys: Vec<String> = [11, 12, 7]
                    .into_iter()
                    .filter_map(|key| {
                        let id = text_at(&fields, key);
                        (!id.is_empty()).then(|| format!("antigravity:{key}:{id}"))
                    })
                    .collect();
                let learned = models.get(&model_id).cloned().flatten();
                let resolved = if is_concrete_model(&model) {
                    model
                } else {
                    learned
                        .or_else(|| offline_model(model_id).map(str::to_owned))
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| {
                            if model_id > 0 {
                                format!("antigravity-model-{model_id}")
                            } else if !model.is_empty() {
                                model
                            } else {
                                "antigravity-unknown".into()
                            }
                        })
                };
                let rate = rate_model(&resolved);
                records.push(Candidate {
                    record: UsageRecord {
                        provider: "antigravity",
                        session_id: session_id.to_string(),
                        timestamp_ms: entry
                            .timestamp_ms
                            .or(database.trajectory_timestamp)
                            .unwrap_or_default(),
                        rate_model: (rate != resolved).then_some(rate),
                        model: resolved,
                        totals,
                        reported_cost_usd: None,
                        speed: "standard",
                        dedupe_key: keys.first().cloned().unwrap_or_else(|| {
                            format!("antigravity:{session_id}:{source}:{index}:{usage_index}")
                        }),
                    },
                    file_index: 0,
                });
            }
        }
    }
    records
}

// ---------------------------------------------------------------------------
// Sign-in (T3's antigravityAuthSupport.ts and AntigravityAuth.ts)

/// The line `agy acp` writes when it wants a browser sign-in.
pub const ANTIGRAVITY_AUTH_STDOUT_PREFIX: &str =
    "Open the following link to authenticate the ACP server: ";
/// The browser helper's stderr marker.
pub const ANTIGRAVITY_AUTH_BROWSER_MARKER: &str = "__T3_ANTIGRAVITY_AUTH_URL__";
pub const ANTIGRAVITY_SIGN_IN_REQUIRED_MESSAGE: &str =
    "Sign in to Antigravity in Settings before you continue.";
pub const ANTIGRAVITY_CODE_REDIRECT_URI: &str = "https://antigravity.google/oauth-callback";
pub const ANTIGRAVITY_AUTH_TIMEOUT_MS: i64 = 300_000;
const MAX_AUTHORIZATION_URL_LENGTH: usize = 16_384;

/// Which redirect shape the agent asked for: a local loopback listener or the
/// Antigravity page's code login.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravityAuthorizationKind {
    Loopback,
    Code,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntigravityAuthorization {
    pub authorization_url: String,
    pub redirect_uri: String,
    pub state: String,
    pub kind: AntigravityAuthorizationKind,
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            && let Ok(hex) = u8::from_str_radix(&value[index + 1..index + 3], 16)
        {
            out.push(hex);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Query pairs with their count, so duplicate `state`/`code` parameters can
/// be refused as T3 does.
fn query_pairs(url: &str) -> Vec<(String, String)> {
    let query = url.split_once('?').map(|(_, query)| query).unwrap_or("");
    let query = query.split('#').next().unwrap_or(query);
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(key), percent_decode(value))
        })
        .collect()
}

fn single(pairs: &[(String, String)], key: &str) -> Option<String> {
    let values = pairs.iter().filter(|(candidate, _)| candidate == key);
    let mut values = values;
    let first = values.next()?.1.clone();
    values.next().is_none().then_some(first)
}

/// T3's `parseAntigravityAuthorizationUrl`. Only Google's OAuth endpoint with
/// the expected redirect shape is accepted.
pub fn parse_antigravity_authorization_url(
    authorization_url: &str,
    kind: AntigravityAuthorizationKind,
) -> Result<AntigravityAuthorization, &'static str> {
    let invalid = "Antigravity returned an invalid Google sign-in URL.";
    if authorization_url.len() > MAX_AUTHORIZATION_URL_LENGTH
        || authorization_url.chars().any(char::is_whitespace)
    {
        return Err(invalid);
    }
    let (origin_path, _) = authorization_url
        .split_once('?')
        .unwrap_or((authorization_url, ""));
    let expected_path = match kind {
        AntigravityAuthorizationKind::Loopback => "/o/oauth2/v2/auth",
        AntigravityAuthorizationKind::Code => "/o/oauth2/auth",
    };
    if origin_path != format!("https://accounts.google.com{expected_path}")
        || authorization_url.contains('#')
    {
        return Err(invalid);
    }
    let pairs = query_pairs(authorization_url);
    let state = single(&pairs, "state").ok_or(invalid)?;
    if state.is_empty() || state.len() > 512 || state.chars().any(char::is_whitespace) {
        return Err(invalid);
    }
    let redirect_uri = single(&pairs, "redirect_uri").ok_or(invalid)?;
    if single(&pairs, "response_type").as_deref() != Some("code") {
        return Err(invalid);
    }
    match kind {
        AntigravityAuthorizationKind::Loopback => {
            let Some((host, port)) = loopback_parts(&redirect_uri) else {
                return Err(invalid);
            };
            if host != "127.0.0.1" || port < 1024 {
                return Err(invalid);
            }
        }
        AntigravityAuthorizationKind::Code => {
            if redirect_uri != ANTIGRAVITY_CODE_REDIRECT_URI {
                return Err(invalid);
            }
        }
    }
    Ok(AntigravityAuthorization {
        authorization_url: authorization_url.to_string(),
        redirect_uri,
        state,
        kind,
    })
}

fn loopback_parts(redirect_uri: &str) -> Option<(&str, u16)> {
    let rest = redirect_uri.strip_prefix("http://")?;
    let (authority, path) = rest.split_once('/')?;
    if !path.is_empty() {
        return None;
    }
    let (host, port) = authority.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    (port > 0).then_some((host, port))
}

/// T3's `antigravityLoginResponse`: a bare code, or the page's full callback
/// URL whose state and code must answer this sign-in.
pub fn antigravity_login_response(
    pending: &AntigravityAuthorization,
    response: &str,
) -> Option<String> {
    let value = response.trim();
    if !value.is_empty()
        && value.len() <= 2048
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '~'))
    {
        return Some(value.to_string());
    }
    if value.len() > MAX_AUTHORIZATION_URL_LENGTH || value.chars().any(char::is_whitespace) {
        return None;
    }
    let (origin_path, _) = value.split_once('?')?;
    if origin_path != pending.redirect_uri {
        return None;
    }
    let pairs = query_pairs(value);
    let state = single(&pairs, "state")?;
    let code = single(&pairs, "code")?;
    (state == pending.state && !code.is_empty()).then(|| value.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AntigravityAuthPhase {
    Idle,
    Starting,
    Waiting,
    Verifying,
    Succeeded,
    Failed,
    Cancelled,
}

/// The state a client sees. Only the owner sees the URL and interaction id.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AntigravityAuthState {
    pub instance_id: String,
    pub phase: AntigravityAuthPhase,
    pub flow_id: Option<String>,
    pub authorization_url: Option<String>,
    pub expires_at: Option<String>,
    pub message: Option<String>,
    /// The waiting flow expects the Antigravity page's code, not a callback.
    pub code_login: bool,
}

#[derive(Clone, Debug)]
struct ActiveAuthFlow {
    id: String,
    owner: String,
    expires_at_ms: i64,
    pending: Option<AntigravityAuthorization>,
    callback_sent: bool,
    login_response: Option<String>,
    code_login: bool,
    phase: AntigravityAuthPhase,
    message: Option<String>,
}

/// T3's `AntigravityAuth` state machine, with the network-free parts. The
/// process still performs the exchange; this records the flow, validates the
/// link and its answer, and gates cancel/logout.
#[derive(Clone, Debug, Default)]
pub struct AntigravityAuthMachine {
    instance_id: String,
    flow: Option<ActiveAuthFlow>,
}

/// The validated callback a host would forward to the loopback listener.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntigravityCallback {
    pub callback_url: String,
    pub kind: AntigravityAuthorizationKind,
}

impl AntigravityAuthMachine {
    pub fn new(instance_id: impl Into<String>) -> Self {
        Self {
            instance_id: instance_id.into(),
            flow: None,
        }
    }

    pub fn state(&self, owner: &str) -> AntigravityAuthState {
        let Some(flow) = &self.flow else {
            return AntigravityAuthState {
                instance_id: self.instance_id.clone(),
                phase: AntigravityAuthPhase::Idle,
                flow_id: None,
                authorization_url: None,
                expires_at: None,
                message: None,
                code_login: false,
            };
        };
        let visible = flow.owner == owner;
        AntigravityAuthState {
            instance_id: self.instance_id.clone(),
            phase: flow.phase,
            flow_id: visible.then(|| flow.id.clone()),
            authorization_url: if visible {
                flow.pending
                    .as_ref()
                    .map(|pending| pending.authorization_url.clone())
            } else {
                None
            },
            expires_at: if visible {
                iso_millis(flow.expires_at_ms)
            } else {
                None
            },
            message: if visible {
                flow.message.clone()
            } else if matches!(
                flow.phase,
                AntigravityAuthPhase::Starting
                    | AntigravityAuthPhase::Waiting
                    | AntigravityAuthPhase::Verifying
            ) {
                Some("Sign-in is in progress in another client.".into())
            } else {
                None
            },
            code_login: visible && flow.code_login,
        }
    }

    /// Starts a flow. Returns the caller's existing flow when it is the
    /// owner, as T3's idempotent `start` does.
    pub fn start(
        &mut self,
        owner: &str,
        flow_id: impl Into<String>,
        now_ms: i64,
    ) -> Result<AntigravityAuthState, &'static str> {
        if let Some(flow) = &self.flow {
            if flow.owner == owner
                && matches!(
                    flow.phase,
                    AntigravityAuthPhase::Starting
                        | AntigravityAuthPhase::Waiting
                        | AntigravityAuthPhase::Verifying
                )
            {
                return Ok(self.state(owner));
            }
            return Err("Antigravity setup is already in progress.");
        }
        self.flow = Some(ActiveAuthFlow {
            id: flow_id.into(),
            owner: owner.to_string(),
            expires_at_ms: now_ms + ANTIGRAVITY_AUTH_TIMEOUT_MS,
            pending: None,
            callback_sent: false,
            login_response: None,
            code_login: false,
            phase: AntigravityAuthPhase::Starting,
            message: Some("Starting Google sign-in.".into()),
        });
        Ok(self.state(owner))
    }

    /// The agent announced its authorization URL.
    pub fn receive_authorization_url(
        &mut self,
        flow_id: &str,
        url: &str,
        kind: AntigravityAuthorizationKind,
        now_ms: i64,
    ) -> Result<AntigravityAuthState, &'static str> {
        let owner = self.require_flow(flow_id, now_ms)?;
        let authorization = parse_antigravity_authorization_url(url, kind)?;
        let flow = self.flow.as_mut().expect("checked flow");
        if let Some(pending) = &flow.pending {
            if pending.authorization_url == authorization.authorization_url {
                return Ok(self.state(&owner));
            }
            return Err("Antigravity started more than one Google sign-in request.");
        }
        flow.pending = Some(authorization);
        flow.code_login = kind == AntigravityAuthorizationKind::Code;
        flow.phase = AntigravityAuthPhase::Waiting;
        flow.message = Some(
            if flow.code_login {
                "Open the Google sign-in link, then paste the code from the Antigravity page here."
            } else {
                "Open the Google sign-in link. If you are remote, paste the redirect URL here."
            }
            .into(),
        );
        Ok(self.state(&owner))
    }

    /// Validates the redirect a person pasted for a loopback flow. Forwarding
    /// it to the agent's local listener is the host's job (a gap here).
    pub fn complete(
        &mut self,
        flow_id: &str,
        callback_url: &str,
        now_ms: i64,
    ) -> Result<AntigravityCallback, &'static str> {
        let owner = self.require_flow(flow_id, now_ms)?;
        let flow = self.flow.as_mut().expect("checked flow");
        let pending = flow
            .pending
            .clone()
            .ok_or("Wait for the Google sign-in link before you send a redirect URL.")?;
        if flow.callback_sent {
            return Err("The sign-in response was already sent. Wait for Google to finish.");
        }
        if pending.kind != AntigravityAuthorizationKind::Loopback {
            return Err("This sign-in expects the code from the Antigravity page.");
        }
        let Some(callback) = antigravity_login_response(&pending, callback_url) else {
            return Err("Paste the redirect URL from the Google sign-in page.");
        };
        flow.callback_sent = true;
        flow.phase = AntigravityAuthPhase::Verifying;
        flow.message = Some("Waiting for Google to finish sign-in.".into());
        let _ = owner;
        Ok(AntigravityCallback {
            callback_url: callback,
            kind: pending.kind,
        })
    }

    /// Validates the code (or callback URL) for a code-login flow and returns
    /// what the host submits to the agent's login.
    pub fn respond(
        &mut self,
        flow_id: &str,
        response: &str,
        now_ms: i64,
    ) -> Result<String, &'static str> {
        let owner = self.require_flow(flow_id, now_ms)?;
        let flow = self.flow.as_mut().expect("checked flow");
        let pending = flow
            .pending
            .clone()
            .ok_or("Wait for the Google sign-in link before you send a code.")?;
        if flow.callback_sent {
            return Err("The sign-in response was already sent. Wait for Google to finish.");
        }
        if !flow.code_login {
            return Err("This sign-in expects the redirect URL, not a code.");
        }
        let Some(value) = antigravity_login_response(&pending, response) else {
            return Err("Paste the code from the Antigravity page, or that page's full URL.");
        };
        flow.login_response = Some(value.clone());
        flow.callback_sent = true;
        flow.phase = AntigravityAuthPhase::Verifying;
        flow.message = Some("Waiting for Google to finish sign-in.".into());
        let _ = owner;
        Ok(value)
    }

    /// The host verified access and models.
    pub fn authenticated(&mut self) -> Option<AntigravityAuthState> {
        let owner = {
            let flow = self.flow.as_mut()?;
            flow.phase = AntigravityAuthPhase::Succeeded;
            flow.pending = None;
            flow.message = Some("Signed in with Google.".into());
            flow.owner.clone()
        };
        Some(self.state(&owner))
    }

    pub fn fail(&mut self, message: &str) -> Option<AntigravityAuthState> {
        let owner = {
            let flow = self.flow.as_mut()?;
            flow.phase = AntigravityAuthPhase::Failed;
            flow.pending = None;
            flow.message = Some(message.to_string());
            flow.owner.clone()
        };
        Some(self.state(&owner))
    }

    pub fn cancel(&mut self) -> Option<AntigravityAuthState> {
        let owner = {
            let flow = self.flow.as_mut()?;
            flow.phase = AntigravityAuthPhase::Cancelled;
            flow.pending = None;
            flow.message = Some("Google sign-in was cancelled.".into());
            flow.owner.clone()
        };
        Some(self.state(&owner))
    }

    /// Logout stops any active flow, then clears the local record. The agent
    /// itself performs the credential removal through ACP `logout`.
    pub fn logout(&mut self) -> Option<AntigravityAuthState> {
        if let Some(flow) = self.flow.as_mut() {
            flow.phase = AntigravityAuthPhase::Cancelled;
            flow.pending = None;
            flow.message = Some("Google sign-in was cancelled by sign-out.".into());
        }
        self.flow = None;
        Some(AntigravityAuthState {
            instance_id: self.instance_id.clone(),
            phase: AntigravityAuthPhase::Idle,
            flow_id: None,
            authorization_url: None,
            expires_at: None,
            message: Some("Signed out of Google.".into()),
            code_login: false,
        })
    }

    fn require_flow(&self, flow_id: &str, now_ms: i64) -> Result<String, &'static str> {
        let flow = self
            .flow
            .as_ref()
            .filter(|flow| flow.id == flow_id)
            .ok_or("This sign-in is no longer active in this client.")?;
        if now_ms >= flow.expires_at_ms {
            return Err("Google sign-in expired. Start sign-in again.");
        }
        Ok(flow.owner.clone())
    }
}

fn iso_millis(ms: i64) -> Option<String> {
    jiff::Timestamp::from_millisecond(ms)
        .ok()
        .map(|timestamp| timestamp.to_string())
}

/// T3's `antigravityProfileSettings`: the selected method, never a credential.
pub fn antigravity_profile_settings(auth: &AntigravityAuthConfig) -> String {
    let mut gcp = serde_json::Map::new();
    if !auth.gcp_project.is_empty() {
        gcp.insert("project".into(), json!(auth.gcp_project));
    }
    if !auth.gcp_location.is_empty() {
        gcp.insert("location".into(), json!(auth.gcp_location));
    }
    let mut profile = serde_json::Map::new();
    profile.insert("auth".into(), json!({ "type": auth.auth_method }));
    if !gcp.is_empty() {
        profile.insert("gcp".into(), Value::Object(gcp));
    }
    format!("{}\n", Value::Object(profile))
}

/// The isolated Antigravity profile an instance runs with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AntigravityProfile {
    pub gemini_home: PathBuf,
    pub acp_directory: PathBuf,
    pub token_path: PathBuf,
    pub temp_directory: PathBuf,
    pub browser_command: String,
    pub ca_bundle_path: Option<PathBuf>,
}

/// T3's `antigravityEnvironment`: a clean profile environment. Credentials
/// for key methods are passed through; every inherited Google key is removed
/// so the selected method is the only one the agent can use.
pub fn antigravity_environment(
    profile: &AntigravityProfile,
    base_env: &[(String, String)],
    auth: &AntigravityAuthConfig,
) -> Vec<(String, String)> {
    const REMOVED: &[&str] = &[
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
        "GOOGLE_APPLICATION_CREDENTIALS",
        "GOOGLE_CLOUD_PROJECT",
        "GOOGLE_CLOUD_LOCATION",
        "GOOGLE_CLOUD_QUOTA_PROJECT",
        "GOOGLE_GENAI_USE_VERTEXAI",
        "GCLOUD_PROJECT",
        "CLOUDSDK_CORE_PROJECT",
        "AGY_ACP_CCPA_PROJECT",
        "AGY_ACP_ENABLE_OAUTH",
        "GEMINI_HOME",
        "AGY_ACP_FORCE_FILE_STORAGE",
        "AGY_CLI_DISABLE_AUTO_UPDATE",
        "BROWSER",
        "PYTHONUNBUFFERED",
        "ELECTRON_RUN_AS_NODE",
        "CHROME_CRASHPAD_PIPE_NAME",
    ];
    let mut environment: Vec<(String, String)> = base_env
        .iter()
        .filter(|(key, _)| {
            let upper = key.to_ascii_uppercase();
            !REMOVED.iter().any(|removed| *removed == upper)
        })
        .cloned()
        .collect();
    if auth.auth_method == "gemini-api-key" && !auth.api_key.is_empty() {
        environment.push(("GEMINI_API_KEY".into(), auth.api_key.clone()));
    }
    if auth.auth_method == "agent-platform" && !auth.api_key.is_empty() {
        environment.push(("GOOGLE_API_KEY".into(), auth.api_key.clone()));
    }
    if let Some(ca_bundle) = &profile.ca_bundle_path
        && !base_env.iter().any(|(key, _)| {
            key.eq_ignore_ascii_case("SSL_CERT_FILE") || key.eq_ignore_ascii_case("SSL_CERT_DIR")
        })
    {
        environment.push(("SSL_CERT_FILE".into(), ca_bundle.display().to_string()));
    }
    if auth.auth_method == "oauth-personal"
        && !base_env.iter().any(|(key, _)| key == "SSH_CONNECTION")
    {
        environment.push(("SSH_CONNECTION".into(), "127.0.0.1 0 127.0.0.1 0".into()));
    }
    environment.extend([
        (
            "GEMINI_HOME".into(),
            profile.gemini_home.display().to_string(),
        ),
        ("AGY_ACP_FORCE_FILE_STORAGE".into(), "1".into()),
        ("AGY_CLI_DISABLE_AUTO_UPDATE".into(), "1".into()),
        ("BROWSER".into(), profile.browser_command.clone()),
        ("PYTHONUNBUFFERED".into(), "1".into()),
        ("ELECTRON_RUN_AS_NODE".into(), "1".into()),
        (
            "TMPDIR".into(),
            profile.temp_directory.display().to_string(),
        ),
    ]);
    environment
}

/// Creates the isolated profile directories and writes its method settings.
/// Linking user skills and verifying browser suppression stay real-machine
/// concerns (gaps in this slice).
pub fn prepare_antigravity_profile(
    profile_directory: &Path,
    auth: &AntigravityAuthConfig,
) -> Result<AntigravityProfile, String> {
    let gemini_home = profile_directory.to_path_buf();
    let acp_directory = gemini_home.join("antigravity-acp");
    let temp_directory = acp_directory.join("tmp");
    for directory in [&gemini_home, &acp_directory, &temp_directory] {
        fs::create_dir_all(directory)
            .map_err(|_| "The Antigravity profile directory could not be created.".to_string())?;
    }
    fs::write(
        acp_directory.join("settings.json"),
        antigravity_profile_settings(auth),
    )
    .map_err(|_| "The Antigravity profile settings could not be written.".to_string())?;
    Ok(AntigravityProfile {
        token_path: acp_directory.join("acp_token.json"),
        gemini_home,
        acp_directory,
        temp_directory,
        browser_command: String::new(),
        ca_bundle_path: None,
    })
}

// ---------------------------------------------------------------------------
// Prompt attachments (T3's AntigravityAcpSupport.ts `buildAntigravityPrompt`)

/// What the agent's handshake said it accepts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PromptCapabilities {
    pub images: bool,
    pub audio: bool,
    pub embedded_context: bool,
}

pub const ANTIGRAVITY_MAX_AUDIO_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
pub const ANTIGRAVITY_MAX_TEXT_ATTACHMENT_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_ATTACHMENT_BYTES: usize = 50 * 1024 * 1024;
pub const PROVIDER_SEND_TURN_MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;

fn image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/bmp" | "image/jpeg" | "image/png" | "image/webp"
    )
}

fn audio_mime(mime: &str) -> bool {
    matches!(
        mime,
        "audio/aac"
            | "audio/flac"
            | "audio/mp3"
            | "audio/mpeg"
            | "audio/mp4"
            | "audio/m4a"
            | "audio/x-m4a"
            | "audio/ogg"
            | "audio/wav"
            | "audio/x-wav"
            | "audio/webm"
    )
}

fn text_mime(mime: &str) -> bool {
    mime.starts_with("text/")
        || matches!(
            mime,
            "application/json"
                | "application/ld+json"
                | "application/javascript"
                | "application/typescript"
                | "application/xml"
                | "application/yaml"
                | "application/x-yaml"
                | "application/x-sh"
        )
}

fn text_file_extension(name: &str) -> bool {
    let extension = Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        extension.as_str(),
        "txt"
            | "md"
            | "mdx"
            | "json"
            | "jsonl"
            | "yaml"
            | "yml"
            | "toml"
            | "xml"
            | "csv"
            | "tsv"
            | "js"
            | "jsx"
            | "mjs"
            | "cjs"
            | "ts"
            | "tsx"
            | "html"
            | "css"
            | "scss"
            | "less"
            | "py"
            | "rs"
            | "go"
            | "java"
            | "kt"
            | "swift"
            | "cc"
            | "cpp"
            | "hpp"
            | "cs"
            | "rb"
            | "php"
            | "sh"
            | "bash"
            | "zsh"
            | "sql"
            | "graphql"
            | "svelte"
            | "vue"
            | "log"
            | "diff"
            | "patch"
            | "ini"
            | "conf"
    )
}

fn decode_text(bytes: &[u8]) -> Result<String, String> {
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (pairs, _) = bytes[2..].as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|pair| u16::from_le_bytes(*pair)).collect();
        return String::from_utf16(&units).map_err(|_| "not a UTF-16LE text file".to_string());
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (pairs, _) = bytes[2..].as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect();
        return String::from_utf16(&units).map_err(|_| "not a UTF-16BE text file".to_string());
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| "not a UTF-8 text file".to_string())
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}

/// T3's `buildAntigravityPrompt`: text plus native image/audio blocks, an
/// embedded `resource` for text files, a `resource_link` for PDFs, and a
/// path-only note for anything the agent did not negotiate.
pub(crate) fn build_prompt_blocks(
    text: &str,
    attachments: &[super::acp::ResolvedAttachment],
    capabilities: PromptCapabilities,
) -> Result<Vec<Value>, String> {
    let mut blocks = Vec::new();
    let text = text.trim();
    if !text.is_empty() {
        blocks.push(json!({ "type": "text", "text": text }));
    }
    let mut total_bytes = 0usize;
    for attachment in attachments {
        let path = attachment
            .path
            .as_deref()
            .ok_or_else(|| format!("Invalid attachment '{}'.", attachment.name))?;
        let metadata = fs::metadata(path)
            .map_err(|_| format!("Could not read attachment '{}'.", attachment.name))?;
        if !metadata.is_file() {
            return Err(format!("Could not read attachment '{}'.", attachment.name));
        }
        let mime_type = attachment
            .mime_type
            .to_ascii_lowercase()
            .split(';')
            .next()
            .unwrap_or_default()
            .to_string();
        let image = attachment.image && image_mime(&mime_type);
        let audio = !attachment.image && audio_mime(&mime_type);
        let pdf = !attachment.image && mime_type == "application/pdf";
        let text_file =
            !attachment.image && (text_mime(&mime_type) || text_file_extension(&attachment.name));
        let path_only = (attachment.image && (!image || !capabilities.images))
            || (!attachment.image
                && (attachment.pasted_text
                    || if audio {
                        !capabilities.audio
                    } else {
                        !(pdf || text_file) || !capabilities.embedded_context
                    }));
        if path_only {
            continue;
        }
        let size = usize::try_from(metadata.len()).unwrap_or(usize::MAX);
        let limit = if image {
            PROVIDER_SEND_TURN_MAX_IMAGE_BYTES
        } else if audio {
            ANTIGRAVITY_MAX_AUDIO_ATTACHMENT_BYTES
        } else if pdf {
            MAX_TOTAL_ATTACHMENT_BYTES
        } else {
            ANTIGRAVITY_MAX_TEXT_ATTACHMENT_BYTES
        };
        // T3 skips an oversized file but refuses an oversized image.
        if !attachment.image && (size > limit || total_bytes + size > MAX_TOTAL_ATTACHMENT_BYTES) {
            continue;
        }
        total_bytes += size;
        if size > limit || total_bytes > MAX_TOTAL_ATTACHMENT_BYTES {
            return Err(format!(
                "Image '{}' is too large. Antigravity accepts images up to 10 MiB and 50 MiB of native attachments per message.",
                attachment.name
            ));
        }
        if pdf {
            blocks.push(json!({
                "type": "resource_link",
                "name": attachment.name,
                "mimeType": mime_type,
                "uri": file_url(path),
            }));
            continue;
        }
        let bytes = fs::read(path)
            .map_err(|_| format!("Could not read attachment '{}'.", attachment.name))?;
        if bytes.len() > limit
            || total_bytes + bytes.len().saturating_sub(size) > MAX_TOTAL_ATTACHMENT_BYTES
        {
            return Err(format!(
                "Attachment '{}' changed while being read and is too large.",
                attachment.name
            ));
        }
        if image {
            blocks.push(json!({
                "type": "image",
                "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "mimeType": mime_type,
            }));
        } else if audio {
            blocks.push(json!({
                "type": "audio",
                "data": base64::engine::general_purpose::STANDARD.encode(&bytes),
                "mimeType": mime_type,
            }));
        } else {
            let decoded = decode_text(&bytes)
                .map_err(|kind| format!("Attachment '{}' is {kind}.", attachment.name))?;
            if decoded.contains('\0') {
                return Err(format!(
                    "Attachment '{}' contains binary data.",
                    attachment.name
                ));
            }
            blocks.push(json!({
                "type": "resource",
                "resource": { "mimeType": mime_type, "text": decoded },
            }));
        }
    }
    if blocks.is_empty() {
        return Err("A turn requires text or supported attachments.".into());
    }
    Ok(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn varint(mut value: u64) -> Vec<u8> {
        let mut bytes = Vec::new();
        loop {
            let byte = (value % 128) as u8;
            value /= 128;
            if value > 0 {
                bytes.push(byte + 128);
            } else {
                bytes.push(byte);
                return bytes;
            }
        }
    }

    fn proto_varint(number: u64, value: u64) -> Vec<u8> {
        let mut encoded = varint(number * 8);
        encoded.extend(varint(value));
        encoded
    }

    fn proto_bytes(number: u64, payload: &[u8]) -> Vec<u8> {
        let mut encoded = varint(number * 8 + 2);
        encoded.extend(varint(payload.len() as u64));
        encoded.extend_from_slice(payload);
        encoded
    }

    fn proto_text(number: u64, text: &str) -> Vec<u8> {
        proto_bytes(number, text.as_bytes())
    }

    fn seed(path: &Path, generations: &[(i64, Vec<u8>)], steps: &[(i64, Vec<u8>)]) {
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE gen_metadata (idx INTEGER, data BLOB);\
                 CREATE TABLE steps (idx INTEGER, metadata BLOB)",
            )
            .unwrap();
        for (index, data) in generations {
            connection
                .execute(
                    "INSERT INTO gen_metadata VALUES (?1, ?2)",
                    rusqlite::params![index, data],
                )
                .unwrap();
        }
        for (index, data) in steps {
            connection
                .execute(
                    "INSERT INTO steps VALUES (?1, ?2)",
                    rusqlite::params![index, data],
                )
                .unwrap();
        }
    }

    // Ported from apps/server/src/provider/Drivers/AntigravitySkills.test.ts and
    // AntigravitySkills.ts (T3 Code v0.1.11, MIT).
    #[test]
    fn skills_read_frontmatter_sorted_and_deduplicated() {
        let home = tempdir().unwrap();
        let cwd = tempdir().unwrap();
        let user = home.path().join(".gemini/config/skills");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(user.join("alpha")).unwrap();
        fs::write(
            user.join("alpha/SKILL.md"),
            "---\nname: deploy\ndescription: Ship it\n---\nbody",
        )
        .unwrap();
        // A project skill with the same name does not replace the user's.
        fs::create_dir_all(cwd.path().join(".agents/skills/beta")).unwrap();
        fs::write(
            cwd.path().join(".agents/skills/beta/skill.md"),
            "---\nname: deploy\ndescription: Other\n---\n",
        )
        .unwrap();
        fs::create_dir_all(cwd.path().join(".gemini/skills/gamma")).unwrap();
        fs::write(
            cwd.path().join(".gemini/skills/gamma/skill.md"),
            "---\nname: review\n---\n",
        )
        .unwrap();
        let group = discover_skills(cwd.path().to_str().unwrap(), home.path(), Some(home.path()));
        let names: Vec<&str> = group
            .skills
            .iter()
            .filter_map(|skill| skill["name"].as_str())
            .collect();
        assert_eq!(names, vec!["deploy", "review"]);
        let deploy = group
            .skills
            .iter()
            .find(|skill| skill["name"] == "deploy")
            .unwrap();
        assert_eq!(deploy["description"], "Ship it");
        assert_eq!(deploy["scope"], "user");
        assert_eq!(deploy["enabled"], true);
        assert!(group.errors.is_empty());
    }

    // Ported from apps/server/src/provider/acp/AntigravityProtocol.test.ts
    // and AntigravityChildWork.ts (T3 Code v0.1.11, MIT).
    #[test]
    fn subagent_launches_read_flat_and_batched_inputs() {
        let flat = json!({"Role": "Researcher", "Prompt": "Read the README"});
        assert_eq!(
            subagent_launches(&flat).unwrap(),
            vec![SubagentLaunch {
                role: "Researcher".into(),
                prompt: "Read the README".into(),
            }]
        );
        let batch = json!({"Subagents": [
            {"Role": "A", "Prompt": "one"},
            {"Role": "B", "Prompt": "two"}
        ]});
        let launches = subagent_launches(&batch).unwrap();
        assert_eq!(launches.len(), 2);
        assert_eq!(
            describe_subagent_launches(&launches),
            ("A, B".into(), "A\none\n\nB\ntwo".into())
        );
        assert!(subagent_launches(&json!({"Subagents": []})).is_none());
        assert!(subagent_launches(&json!({"Role": "A"})).is_none());
    }

    // Ported from apps/server/src/provider/antigravityAuthSupport.ts
    // (T3 Code v0.1.11, MIT).
    #[test]
    fn auth_methods_and_validation_match_the_installed_cli() {
        assert_eq!(AntigravityAuthMethod::ALL.len(), 4);
        assert_eq!(AntigravityAuthMethod::ALL[0].id, "oauth-personal");
        assert!(auth_uses_browser("oauth-personal"));
        assert!(auth_uses_browser("oauth-business"));
        assert!(!auth_uses_browser("gemini-api-key"));
        assert_eq!(auth_label("agent-platform"), "Agent Platform");
        assert_eq!(
            auth_config_issue(&AntigravityAuthConfig {
                auth_method: "oauth-business".into(),
                ..Default::default()
            }),
            Some(
                "Gemini Enterprise needs a GCP project and location in the Antigravity provider settings."
            )
        );
        assert_eq!(
            auth_config_issue(&AntigravityAuthConfig {
                auth_method: "gemini-api-key".into(),
                api_key: "key".into(),
                ..Default::default()
            }),
            None
        );
    }

    // Ported from apps/server/src/provider/Drivers/antigravityUsageReader.ts
    // (T3 Code v0.1.11, MIT): numeric ids are learned from another database in
    // the same scan, and effort variants use the base model's rate.
    #[test]
    fn usage_reader_learns_numeric_models_from_other_databases() {
        let dir = tempdir().unwrap();
        let usage = dir.path().join("a-usage.db");
        seed(
            &usage,
            &[],
            &[
                (
                    0,
                    proto_bytes(9, &{
                        let mut usage = proto_varint(1, 9001);
                        usage.extend(proto_varint(2, 10));
                        usage
                    }),
                ),
                (
                    1,
                    proto_bytes(9, &{
                        let mut usage = proto_varint(1, 9002);
                        usage.extend(proto_varint(2, 10));
                        usage
                    }),
                ),
            ],
        );
        let first = AntigravityUsageReader::default().scan(&[dir.path().to_path_buf()], 0);
        let models: Vec<&str> = first
            .files
            .iter()
            .flat_map(|file| file.records.iter())
            .map(|record| record.model.as_str())
            .collect();
        assert_eq!(
            models,
            vec!["antigravity-model-9001", "antigravity-model-9002"]
        );

        let evidence = dir.path().join("z-names.db");
        seed(
            &evidence,
            &[
                (
                    0,
                    proto_bytes(1, &{
                        let mut data = proto_varint(3, 9001);
                        data.extend(proto_text(21, "Gemini 9.1 Flash (High)"));
                        data
                    }),
                ),
                (
                    1,
                    proto_bytes(1, &{
                        let mut data = proto_varint(3, 9002);
                        data.extend(proto_text(19, "gemini-default"));
                        data
                    }),
                ),
            ],
            &[],
        );
        let next = AntigravityUsageReader::default().scan(&[dir.path().to_path_buf()], 0);
        let records: Vec<(String, Option<String>)> = next
            .files
            .iter()
            .flat_map(|file| file.records.iter())
            .map(|record| (record.model.clone(), record.rate_model.clone()))
            .collect();
        assert_eq!(
            records,
            vec![
                (
                    "gemini-9.1-flash-high".into(),
                    Some("gemini-9.1-flash".into())
                ),
                ("antigravity-model-9002".into(), None),
            ]
        );
    }

    // Ported from apps/server/src/provider/Drivers/antigravityUsageReader.ts
    // `modelName`, `isConcreteModel` and `antigravityRateModel`
    // (T3 Code v0.1.11, MIT).
    #[test]
    fn model_names_rate_models_and_aliases_follow_t3() {
        assert_eq!(
            model_name("Gemini 9.1 Flash (High)"),
            "gemini-9.1-flash-high"
        );
        assert_eq!(
            model_name("Claude 4.5 Sonnet (Thinking)"),
            "claude-sonnet-4-5-thinking"
        );
        assert_eq!(model_name("gemini-default"), "gemini-default");
        assert!(is_concrete_model("gemini-3.1-pro"));
        assert!(is_concrete_model("claude-sonnet-4-5"));
        assert!(!is_concrete_model("gemini-default"));
        assert!(!is_concrete_model("gemini-3-flash-a"));
        assert_eq!(rate_model("gemini-9.1-flash-high"), "gemini-9.1-flash");
        assert_eq!(rate_model("gemini-3-flash"), "gemini-3-flash-preview");
        assert_eq!(rate_model("gemini-3-flash-a"), "gemini-3.5-flash");
    }

    // Ported from apps/server/src/provider/antigravityAuthSupport.ts
    // `parseAntigravityAuthorizationUrl` and `antigravityLoginResponse`
    // (T3 Code v0.1.11, MIT).
    #[test]
    fn antigravity_authorization_urls_and_answers_are_validated() {
        let loopback = "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&state=abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A4567%2F";
        let parsed =
            parse_antigravity_authorization_url(loopback, AntigravityAuthorizationKind::Loopback)
                .unwrap();
        assert_eq!(parsed.state, "abc");
        assert_eq!(parsed.redirect_uri, "http://127.0.0.1:4567/");
        assert_eq!(parsed.kind, AntigravityAuthorizationKind::Loopback);

        let code = "https://accounts.google.com/o/oauth2/auth?response_type=code&state=abc&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback";
        let parsed =
            parse_antigravity_authorization_url(code, AntigravityAuthorizationKind::Code).unwrap();
        assert_eq!(parsed.kind, AntigravityAuthorizationKind::Code);

        // The wrong endpoint, a privileged port, and a missing state fail.
        assert!(
            parse_antigravity_authorization_url(
                "https://accounts.google.com/o/oauth2/auth?response_type=code&state=abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A4567%2F",
                AntigravityAuthorizationKind::Loopback
            )
            .is_err()
        );
        assert!(
            parse_antigravity_authorization_url(
                "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&state=abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A80%2F",
                AntigravityAuthorizationKind::Loopback
            )
            .is_err()
        );
        assert!(
            parse_antigravity_authorization_url(
                "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&redirect_uri=http%3A%2F%2F127.0.0.1%3A4567%2F",
                AntigravityAuthorizationKind::Loopback
            )
            .is_err()
        );

        assert_eq!(
            antigravity_login_response(&parsed, " 4/0AbCd-123 "),
            Some("4/0AbCd-123".into())
        );
        assert_eq!(
            antigravity_login_response(
                &parsed,
                "https://antigravity.google/oauth-callback?state=abc&code=xyz"
            ),
            Some("https://antigravity.google/oauth-callback?state=abc&code=xyz".into())
        );
        assert_eq!(
            antigravity_login_response(
                &parsed,
                "https://antigravity.google/oauth-callback?state=wrong&code=xyz"
            ),
            None
        );
    }

    #[test]
    fn auth_machine_tracks_a_code_login_flow_and_refuses_replays() {
        let mut machine = AntigravityAuthMachine::new("antigravity");
        let started = machine.start("client-a", "flow-1", 1_000).unwrap();
        assert_eq!(started.phase, AntigravityAuthPhase::Starting);
        // Another client cannot start while this one owns the flow.
        assert!(machine.start("client-b", "flow-2", 1_000).is_err());

        let code = "https://accounts.google.com/o/oauth2/auth?response_type=code&state=abc&redirect_uri=https%3A%2F%2Fantigravity.google%2Foauth-callback";
        let waiting = machine
            .receive_authorization_url("flow-1", code, AntigravityAuthorizationKind::Code, 1_000)
            .unwrap();
        assert_eq!(waiting.phase, AntigravityAuthPhase::Waiting);
        assert!(waiting.code_login);
        // The other client sees the flow is busy, without its URL.
        assert_eq!(machine.state("client-b").authorization_url, None);
        assert_eq!(
            machine.state("client-b").message.as_deref(),
            Some("Sign-in is in progress in another client.")
        );

        let submitted = machine.respond("flow-1", "4/0AbCd", 1_000).unwrap();
        assert_eq!(submitted, "4/0AbCd");
        assert!(machine.respond("flow-1", "4/0AbCd", 1_000).is_err());
        let done = machine.authenticated().unwrap();
        assert_eq!(done.phase, AntigravityAuthPhase::Succeeded);

        // Expired flows are refused before use.
        let mut expired = AntigravityAuthMachine::new("antigravity");
        expired.start("client-a", "flow-3", 0).unwrap();
        let url = "https://accounts.google.com/o/oauth2/v2/auth?response_type=code&state=abc&redirect_uri=http%3A%2F%2F127.0.0.1%3A4567%2F";
        assert!(
            expired
                .receive_authorization_url(
                    "flow-3",
                    url,
                    AntigravityAuthorizationKind::Loopback,
                    ANTIGRAVITY_AUTH_TIMEOUT_MS + 1
                )
                .is_err()
        );
    }

    #[test]
    fn profile_settings_and_environment_isolate_the_selected_method() {
        let auth = AntigravityAuthConfig {
            auth_method: "gemini-api-key".into(),
            api_key: "secret".into(),
            gcp_project: "project".into(),
            gcp_location: "us".into(),
        };
        let settings = antigravity_profile_settings(&auth);
        assert!(settings.contains("\"auth\":{\"type\":\"gemini-api-key\"}"));
        assert!(settings.contains("\"project\":\"project\""));
        assert!(!settings.contains("secret"));

        let profile = AntigravityProfile {
            gemini_home: PathBuf::from("/tmp/profile"),
            acp_directory: PathBuf::from("/tmp/profile/antigravity-acp"),
            token_path: PathBuf::from("/tmp/profile/antigravity-acp/acp_token.json"),
            temp_directory: PathBuf::from("/tmp/profile/antigravity-acp/tmp"),
            browser_command: "helper".into(),
            ca_bundle_path: None,
        };
        let environment = antigravity_environment(
            &profile,
            &[
                ("GEMINI_API_KEY".into(), "inherited".into()),
                ("HOME".into(), "/home/u".into()),
            ],
            &auth,
        );
        let value = |key: &str| {
            environment
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(value("GEMINI_API_KEY"), Some("secret"));
        assert_eq!(value("GEMINI_HOME"), Some("/tmp/profile"));
        assert_eq!(value("HOME"), Some("/home/u"));
        assert_eq!(value("AGY_ACP_FORCE_FILE_STORAGE"), Some("1"));

        let personal = AntigravityAuthConfig {
            auth_method: "oauth-personal".into(),
            ..AntigravityAuthConfig::default()
        };
        let environment = antigravity_environment(&profile, &[], &personal);
        assert!(
            environment.iter().any(|(key, value)| {
                key == "SSH_CONNECTION" && value == "127.0.0.1 0 127.0.0.1 0"
            })
        );
    }

    #[test]
    fn antigravity_prompt_blocks_embed_text_and_images_by_capability() {
        let dir = tempfile::tempdir().unwrap();
        let image_path = dir.path().join("shot.png");
        std::fs::write(&image_path, b"fake-png").unwrap();
        let text_path = dir.path().join("notes.txt");
        std::fs::write(&text_path, b"hello").unwrap();
        let audio_path = dir.path().join("clip.mp3");
        std::fs::write(&audio_path, b"fake-mp3").unwrap();
        let attachments = vec![
            crate::providers::acp::ResolvedAttachment {
                id: "img".into(),
                name: "shot.png".into(),
                mime_type: "image/png".into(),
                image: true,
                pasted_text: false,
                path: Some(image_path),
            },
            crate::providers::acp::ResolvedAttachment {
                id: "txt".into(),
                name: "notes.txt".into(),
                mime_type: "text/plain".into(),
                image: false,
                pasted_text: false,
                path: Some(text_path),
            },
            crate::providers::acp::ResolvedAttachment {
                id: "aud".into(),
                name: "clip.mp3".into(),
                mime_type: "audio/mpeg".into(),
                image: false,
                pasted_text: false,
                path: Some(audio_path),
            },
        ];
        let blocks = build_prompt_blocks(
            "look",
            &attachments,
            PromptCapabilities {
                images: true,
                audio: true,
                embedded_context: true,
            },
        )
        .unwrap();
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[1]["type"], "image");
        assert_eq!(blocks[1]["mimeType"], "image/png");
        assert_eq!(blocks[2]["type"], "resource");
        assert_eq!(blocks[2]["resource"]["text"], "hello");
        assert_eq!(blocks[3]["type"], "audio");

        // Without capabilities, path-only attachments are skipped; the text
        // still sends, and an empty prompt is refused.
        let path_only =
            build_prompt_blocks("look", &attachments, PromptCapabilities::default()).unwrap();
        assert_eq!(path_only.len(), 1);
        assert!(build_prompt_blocks("", &attachments, PromptCapabilities::default()).is_err());
    }
}
