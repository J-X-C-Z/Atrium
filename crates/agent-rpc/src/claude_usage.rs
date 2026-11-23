//! Claude's plan usage for the account signed in on this machine: reads the
//! Claude Code OAuth token (never passed on) and asks Anthropic's usage
//! endpoint, caching the answer. The token reader and the HTTP call are
//! injectable, so nothing here needs the network to be tested.

use std::{
    io::Read,
    sync::Mutex,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// Where usage is read from.
pub const ENDPOINT: &str = "https://api.anthropic.com/api/oauth/usage";
/// How long an answer is reused.
pub const CACHE_FOR: Duration = Duration::from_secs(5 * 60);
/// How long a failure is reused, so a missing token or an outage is not hammered.
const CACHE_FAILURE_FOR: Duration = Duration::from_secs(60);
const BODY_LIMIT: u64 = 256 * 1024;

type Token = dyn Fn() -> Option<String> + Send + Sync;
type Fetch = dyn Fn(&str) -> Result<Value, String> + Send + Sync;

/// A cached reader of Claude's usage.
pub struct ClaudeUsage {
    token: Box<Token>,
    fetch: Box<Fetch>,
    cache: Mutex<Option<(Instant, Option<Value>)>>,
}

impl ClaudeUsage {
    /// Reads this machine's token and asks Anthropic.
    pub fn system() -> Self {
        Self::with(read_token, http_fetch)
    }

    /// With the token source and the HTTP call given.
    pub fn with(
        token: impl Fn() -> Option<String> + Send + Sync + 'static,
        fetch: impl Fn(&str) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            token: Box::new(token),
            fetch: Box::new(fetch),
            cache: Mutex::new(None),
        }
    }

    /// The usage (`five_hour` and `seven_day` with `utilization` and
    /// `resets_at`), or `None` without a token or when the call fails.
    /// Blocks while it fetches.
    pub fn get(&self) -> Option<Value> {
        let mut cache = self.cache.lock().ok()?;
        if let Some((at, value)) = &*cache {
            let ttl = if value.is_some() {
                CACHE_FOR
            } else {
                CACHE_FAILURE_FOR
            };
            if at.elapsed() < ttl {
                return value.clone();
            }
        }
        let value = (self.token)().and_then(|token| match (self.fetch)(&token) {
            Ok(body) => normalize(&body),
            Err(error) => {
                log::debug!("claude usage: {error}");
                None
            }
        });
        *cache = Some((Instant::now(), value.clone()));
        value
    }
}

/// Keeps only the windows, with the two fields each: nothing else of the
/// response, and never anything of the request.
pub fn normalize(body: &Value) -> Option<Value> {
    let mut out = serde_json::Map::new();
    for key in ["five_hour", "seven_day"] {
        let window = &body[key];
        if let Some(utilization) = window["utilization"].as_f64().filter(|n| n.is_finite()) {
            out.insert(
                key.into(),
                json!({"utilization": utilization, "resets_at": window["resets_at"].as_str()}),
            );
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

/// The access token in a `.credentials.json` body (or the macOS keychain's
/// copy of it), unless it has expired by `now_ms`.
pub fn token_from_credentials(text: &str, now_ms: u64) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    let oauth = &value["claudeAiOauth"];
    let token = oauth["accessToken"].as_str().filter(|t| !t.is_empty())?;
    if oauth["expiresAt"].as_u64().is_some_and(|at| at <= now_ms) {
        return None;
    }
    Some(token.to_string())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Claude Code's OAuth token on this machine: `~/.claude/.credentials.json`,
/// or on macOS the keychain item `Claude Code-credentials`.
pub fn read_token() -> Option<String> {
    let now = now_ms();
    if let Some(text) = dirs::home_dir()
        .map(|home| home.join(".claude").join(".credentials.json"))
        .and_then(|path| std::fs::read_to_string(path).ok())
        && let Some(token) = token_from_credentials(&text, now)
    {
        return Some(token);
    }
    if cfg!(target_os = "macos") {
        let output = std::process::Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())?;
        return token_from_credentials(String::from_utf8_lossy(&output.stdout).trim(), now);
    }
    None
}

fn http_fetch(token: &str) -> Result<Value, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(10))
        .redirects(0)
        .build();
    let response = agent
        .get(ENDPOINT)
        .set("Authorization", &format!("Bearer {token}"))
        .set("anthropic-beta", "oauth-2025-04-20")
        .set("Accept", "application/json")
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code}"),
            other => other.to_string().replace(token, "***"),
        })?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(BODY_LIMIT)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    fn body() -> Value {
        json!({
            "five_hour": {"utilization": 12.5, "resets_at": "2025-11-04T04:59:59Z", "extra": 1},
            "seven_day": {"utilization": 40, "resets_at": null},
            "seven_day_opus": {"utilization": 99},
            "access_token": "leak"
        })
    }

    #[test]
    fn normalize_keeps_two_fields_of_two_windows() {
        let out = normalize(&body()).unwrap();
        assert_eq!(
            out,
            json!({
                "five_hour": {"utilization": 12.5, "resets_at": "2025-11-04T04:59:59Z"},
                "seven_day": {"utilization": 40.0, "resets_at": null}
            })
        );
        assert!(normalize(&json!({"error": "x"})).is_none());
    }

    #[test]
    fn credentials_give_a_token_until_it_expires() {
        let text = r#"{"claudeAiOauth":{"accessToken":"tok","expiresAt":2000}}"#;
        assert_eq!(token_from_credentials(text, 1000).as_deref(), Some("tok"));
        assert_eq!(token_from_credentials(text, 2000), None);
        assert_eq!(
            token_from_credentials(r#"{"claudeAiOauth":{"accessToken":"t"}}"#, 5).as_deref(),
            Some("t")
        );
        assert_eq!(token_from_credentials(r#"{"claudeAiOauth":{}}"#, 5), None);
        assert_eq!(token_from_credentials("not json", 5), None);
    }

    #[test]
    fn answers_are_cached_and_failures_give_none() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let usage = ClaudeUsage::with(
            || Some("tok".into()),
            move |token| {
                assert_eq!(token, "tok");
                counted.fetch_add(1, Ordering::SeqCst);
                Ok(body())
            },
        );
        assert!(usage.get().is_some());
        assert!(usage.get().is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let none = ClaudeUsage::with(|| None, |_| panic!("no token, no call"));
        assert!(none.get().is_none());
        let failing = ClaudeUsage::with(|| Some("t".into()), |_| Err("boom".into()));
        assert!(failing.get().is_none());
    }
}
