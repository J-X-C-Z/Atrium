//! A small client for a Hindsight server's REST API: keep a conversation
//! (`retain`) and find what matters for a new one (`recall`). Blocking:
//! call it off the UI thread.

use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};

use crate::HindsightConfig;

pub struct Hindsight {
    base: String,
    key: Option<String>,
    agent: ureq::Agent,
}

impl Hindsight {
    pub fn new(config: &HindsightConfig) -> Self {
        Self {
            base: config.url.trim_end_matches('/').to_string(),
            key: config.api_key.clone().filter(|k| !k.trim().is_empty()),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(3))
                .timeout(Duration::from_secs(20))
                .build(),
        }
    }

    fn request(&self, method: &str, path: &str) -> ureq::Request {
        let request = self.agent.request(method, &format!("{}{path}", self.base));
        match &self.key {
            Some(key) => request.set("Authorization", &format!("Bearer {key}")),
            None => request,
        }
    }

    /// Whether the server answers.
    pub fn health(&self) -> Result<()> {
        self.request("GET", "/health")
            .call()
            .map(|_| ())
            .map_err(|e| anyhow!("{}: {e}", self.base))
    }

    /// Keeps `content` in `bank`, processed in the background. A
    /// conversation keeps one document id, so sending it again replaces the
    /// older copy instead of adding a second.
    pub fn retain(
        &self,
        bank: &str,
        content: &str,
        document: &str,
        context: &str,
        tags: &[String],
    ) -> Result<()> {
        let body = json!({
            "items": [{
                "content": content,
                "context": context,
                "document_id": document,
                "tags": tags,
                "update_mode": "replace",
            }],
            "async": true,
        });
        self.request(
            "POST",
            &format!("/v1/default/banks/{}/memories", segment(bank)),
        )
        .send_json(body)
        .map(|_| ())
        .map_err(|e| anyhow!("retain in {bank}: {}", describe(e)))
    }

    /// What `bank` holds that bears on `query`, best first, within about
    /// `max_tokens`. A bank that does not exist yet holds nothing.
    pub fn recall(&self, bank: &str, query: &str, max_tokens: usize) -> Result<Vec<String>> {
        let body = json!({ "query": query, "max_tokens": max_tokens, "budget": "low" });
        let response = match self
            .request(
                "POST",
                &format!("/v1/default/banks/{}/memories/recall", segment(bank)),
            )
            .send_json(body)
        {
            Ok(response) => response,
            Err(ureq::Error::Status(404, _)) => return Ok(Vec::new()),
            Err(error) => return Err(anyhow!("recall from {bank}: {}", describe(error))),
        };
        let value: Value = response.into_json().context("recall answer")?;
        Ok(texts(&value))
    }
}

fn texts(value: &Value) -> Vec<String> {
    value["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .filter_map(|r| r["text"].as_str())
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn describe(error: ureq::Error) -> String {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            format!("HTTP {code} {}", body.chars().take(200).collect::<String>())
        }
        ureq::Error::Transport(t) => t.to_string(),
    }
}

/// A bank id, safe in a URL path.
fn segment(bank: &str) -> String {
    bank.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Recalls from every bank, keeping the first copy of each memory, within
/// the config's token budget.
pub fn recall_all(
    config: &HindsightConfig,
    project: Option<&str>,
    query: &str,
) -> Result<Vec<String>> {
    let client = Hindsight::new(config);
    let mut out: Vec<String> = Vec::new();
    let banks = config.banks(project);
    let share = (config.recall_tokens / banks.len().max(1)).max(100);
    let mut last_error = None;
    for bank in banks {
        match client.recall(&bank, query, share) {
            Ok(found) => {
                for text in found {
                    if !out.contains(&text) {
                        out.push(text);
                    }
                }
            }
            Err(error) => last_error = Some(error),
        }
    }
    match (out.is_empty(), last_error) {
        (true, Some(error)) => Err(error),
        _ => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        net::TcpListener,
        sync::mpsc,
    };

    /// A one-request-at-a-time HTTP server that answers each request with
    /// `reply(path)` and passes what it got back to the test.
    fn serve(
        count: usize,
        reply: fn(&str) -> (u16, String),
    ) -> (String, mpsc::Receiver<(String, String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for stream in listener.incoming().take(count) {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut first = String::new();
                reader.read_line(&mut first).unwrap();
                let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
                let (mut length, mut auth) = (0, String::new());
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line.trim().is_empty() {
                        break;
                    }
                    let lower = line.to_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        length = v.trim().parse().unwrap();
                    }
                    if lower.starts_with("authorization:") {
                        auth = line.trim().to_string();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let (code, text) = reply(&path);
                write!(
                    stream,
                    "HTTP/1.1 {code} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{text}",
                    text.len()
                )
                .unwrap();
                tx.send((path, String::from_utf8(body).unwrap(), auth))
                    .unwrap();
            }
        });
        (url, rx)
    }

    #[test]
    fn retain_sends_one_document_per_conversation() {
        let (url, got) = serve(1, |_| (200, "{}".into()));
        let config = HindsightConfig {
            url,
            api_key: Some("k".into()),
            ..Default::default()
        };
        Hindsight::new(&config)
            .retain(
                "atrium app",
                "User: hi",
                "thread-1",
                "Atrium conversation",
                &["codex".into()],
            )
            .unwrap();
        let (path, body, auth) = got.recv().unwrap();
        assert_eq!(path, "/v1/default/banks/atrium-app/memories");
        assert_eq!(auth, "Authorization: Bearer k");
        let body: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(body["items"][0]["document_id"], "thread-1");
        assert_eq!(body["items"][0]["update_mode"], "replace");
        assert_eq!(body["async"], true);
    }

    #[test]
    fn recall_reads_both_banks_and_skips_a_missing_one() {
        let (url, _got) = serve(2, |path| {
            if path.contains("atrium-app-1") {
                (404, "{}".into())
            } else {
                (
                    200,
                    r#"{"results":[{"id":"1","text":"Uses zsh"},{"id":"2","text":" "}]}"#.into(),
                )
            }
        });
        let config = HindsightConfig {
            url,
            ..Default::default()
        };
        let found = recall_all(&config, Some("app-1"), "shell").unwrap();
        assert_eq!(found, ["Uses zsh"]);
    }

    #[test]
    fn a_server_that_is_down_says_so() {
        let config = HindsightConfig {
            url: "http://127.0.0.1:9".into(),
            ..Default::default()
        };
        assert!(recall_all(&config, None, "x").is_err());
    }
}
