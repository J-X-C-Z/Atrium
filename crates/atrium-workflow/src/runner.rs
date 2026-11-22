//! The parts a runner needs beyond the pure state machine: commands, PDF
//! page counts, review verdicts, and the tools a template will need.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{Check, Template, run::CheckResult};

/// A tool or font a template needs, and where to get it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Need {
    pub tool: String,
    pub found: bool,
    pub hint: String,
}

/// What a template needs beyond the app: TeX engines and Chinese fonts.
pub fn environment_needs(template: &Template) -> Vec<Need> {
    let mut tools = Vec::new();
    for step in &template.steps {
        for check in &step.checks {
            let Check::Command { run, .. } = &check.check else {
                continue;
            };
            for tool in ["xelatex", "latexmk", "typst"] {
                if run.contains(tool) && !tools.contains(&tool.to_string()) {
                    tools.push(tool.to_string());
                }
            }
        }
    }
    let mut needs: Vec<Need> = tools
        .into_iter()
        .map(|tool| Need {
            found: find_tool(&tool).is_some(),
            hint: match tool.as_str() {
                "typst" => "Install Typst, or use TeX Live/MacTeX for xelatex.".into(),
                _ => "Install TeX Live or MacTeX; macOS usually has /Library/TeX/texbin.".into(),
            },
            tool,
        })
        .collect();
    // A Chinese font only matters for the typeset result.
    if !needs.is_empty() && template_needs_chinese(template) {
        needs.push(Need {
            tool: "Chinese font".into(),
            found: has_chinese_font(),
            hint: "Install a Chinese font such as Noto Sans CJK SC or Source Han Sans SC.".into(),
        });
    }
    needs
}

fn template_needs_chinese(template: &Template) -> bool {
    template.preamble.chars().any(is_cjk)
        || template.steps.iter().any(|step| {
            step.prompt.chars().any(is_cjk)
                || step.checks.iter().any(|check| {
                    serde_json::to_string(&check.check)
                        .unwrap_or_default()
                        .chars()
                        .any(is_cjk)
                })
        })
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x3400..=0x9fff | 0x20000..=0x2ffff)
}

fn find_tool(tool: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    dirs.extend([
        PathBuf::from("/Library/TeX/texbin"),
        PathBuf::from("/usr/local/texlive"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ]);
    dirs.into_iter()
        .map(|dir| dir.join(tool))
        .find(|path| path.is_file())
}

fn has_chinese_font() -> bool {
    #[cfg(target_os = "macos")]
    {
        return Path::new("/System/Library/Fonts/PingFang.ttc").exists()
            || Path::new("/System/Library/Fonts/STHeiti Light.ttc").exists();
    }
    #[cfg(not(target_os = "macos"))]
    {
        // `fc-list` succeeds with no output when nothing matches.
        Command::new("fc-list")
            .args([":lang=zh", "family"])
            .stderr(std::process::Stdio::null())
            .output()
            .is_ok_and(|out| out.status.success() && !out.stdout.trim_ascii().is_empty())
    }
}

/// Run a `command` check in `dir`. The failure detail keeps the end of its
/// output, where compilers put the useful error.
pub fn command(dir: &Path, run: &str, cwd: Option<&str>, timeout_min: u32) -> CheckResult {
    let work = cwd.map_or_else(|| dir.to_path_buf(), |cwd| dir.join(cwd));
    let what = format!("`{run}` succeeds");
    let output = work.join(".atrium").join(format!(
        "check-{}-{}.log",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis())
    ));
    let _ = fs::create_dir_all(work.join(".atrium"));
    let file = match fs::File::create(&output) {
        Ok(file) => file,
        Err(error) => return failed(what, error.to_string()),
    };
    let file_err = match file.try_clone() {
        Ok(file) => file,
        Err(error) => return failed(what, error.to_string()),
    };
    #[cfg(windows)]
    let mut child = Command::new("cmd")
        .args(["/C", run])
        .current_dir(&work)
        .stdout(file)
        .stderr(file_err)
        .spawn();
    #[cfg(not(windows))]
    let child = Command::new("sh")
        .args(["-c", run])
        .current_dir(&work)
        .stdout(file)
        .stderr(file_err)
        .spawn();
    let mut child = match child {
        Ok(child) => child,
        Err(error) => return failed(what, error.to_string()),
    };
    let deadline = Instant::now() + Duration::from_secs(u64::from(timeout_min.max(1)) * 60);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Err(error) => return failed(what, error.to_string()),
        }
    };
    let text = fs::read_to_string(&output).unwrap_or_default();
    let _ = fs::remove_file(&output);
    let detail = tail(&text, 40, 4096);
    if !timed_out && status.is_some_and(|s| s.success()) {
        CheckResult {
            what,
            passed: true,
            detail: String::new(),
            send_back: None,
        }
    } else if timed_out {
        failed(
            what,
            if detail.is_empty() {
                format!("timed out after {timeout_min} min")
            } else {
                format!("timed out after {timeout_min} min\n{detail}")
            },
        )
    } else {
        failed(what, detail)
    }
}

/// The last `lines` lines, at most `bytes` UTF-8 bytes.
fn tail(text: &str, lines: usize, bytes: usize) -> String {
    let mut tail: Vec<&str> = text.lines().rev().take(lines).collect();
    tail.reverse();
    let mut out = tail.join("\n");
    if out.len() > bytes {
        let mut cut = out.len() - bytes;
        while !out.is_char_boundary(cut) {
            cut += 1;
        }
        out = out[cut..].to_string();
    }
    out
}

fn failed(what: String, detail: String) -> CheckResult {
    CheckResult {
        what,
        passed: false,
        detail,
        send_back: None,
    }
}

/// Count PDF pages from the trailer's `/Count`, with a scan of page objects
/// as a fallback. Enough for reports produced by XeLaTeX and most PDF tools.
pub fn pdf_pages(dir: &Path, path: &str, max: Option<u32>, min: Option<u32>) -> CheckResult {
    let what = match (min, max) {
        (Some(a), Some(b)) => format!("{path} has {a}–{b} pages"),
        (None, Some(b)) => format!("{path} has at most {b} pages"),
        (Some(a), None) => format!("{path} has at least {a} pages"),
        (None, None) => format!("{path} is a valid PDF"),
    };
    let bytes = match fs::read(dir.join(path)) {
        Ok(bytes) => bytes,
        Err(error) => return failed(what, error.to_string()),
    };
    if !bytes.starts_with(b"%PDF-") {
        return failed(what, "not a PDF".into());
    }
    let Some(count) = count_pages(&bytes) else {
        return failed(what, "not a PDF: no page count".into());
    };
    let passed = max.is_none_or(|max| count <= max) && min.is_none_or(|min| count >= min);
    CheckResult {
        what,
        passed,
        detail: if passed {
            String::new()
        } else {
            format!("has {count} pages")
        },
        send_back: None,
    }
}

fn count_pages(bytes: &[u8]) -> Option<u32> {
    let start = bytes.len().saturating_sub(1024 * 1024);
    let tail = String::from_utf8_lossy(&bytes[start..]);
    let count = tail
        .match_indices("/Count")
        .filter_map(|(at, _)| {
            tail[at + 6..]
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .and_then(|n| n.parse::<u32>().ok())
        })
        .max();
    if count.is_some_and(|n| n > 0) {
        return count;
    }
    let all = String::from_utf8_lossy(bytes);
    let pages = all.matches("/Type /Page").count();
    (pages > 0).then_some(pages as u32)
}

/// Ask a reviewer for a JSON verdict and read the last verdict in its answer.
/// `fixes` are the earlier steps the reviewer may name as the cause.
pub fn review_prompt(rubric: &str, files: &[String], fixes: &[String]) -> String {
    let files = files
        .iter()
        .map(|f| format!("- `{f}`"))
        .collect::<Vec<_>>()
        .join("\n");
    let blame = if fixes.is_empty() {
        String::new()
    } else {
        format!(
            " If it fails because of an earlier step, add \"step\": one of {}.",
            fixes
                .iter()
                .map(|f| format!("\"{f}\""))
                .collect::<Vec<_>>()
                .join(", ")
        )
    };
    format!(
        "You are reviewing a workflow step. Read the files below against the rubric. Do not change anything.\n\nRubric:\n{rubric}\n\nFiles:\n{files}\n\nAnswer with one JSON object and nothing after it: {{\"pass\": true or false, \"reasons\": [\"...\"]}}.{blame}"
    )
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub pass: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
    /// The earlier step at fault, when the reviewer names one.
    #[serde(default, alias = "send_back", skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

pub fn review_verdict(answer: &str) -> Option<Verdict> {
    for (at, _) in answer.match_indices('{').rev() {
        if let Some(end) = matching_brace(&answer[at..])
            && let Ok(verdict) = serde_json::from_str::<Verdict>(&answer[at..at + end + 1])
        {
            return Some(verdict);
        }
    }
    None
}

fn matching_brace(text: &str) -> Option<usize> {
    let mut depth = 0;
    let mut quoted = false;
    let mut escaped = false;
    for (ix, c) in text.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                quoted = false;
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(ix);
                }
            }
            _ => {}
        }
    }
    None
}

/// The tail helper is used by logs too.
pub fn log_tail(text: &str) -> String {
    tail(text, 40, 4096)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_keep_the_end_of_their_output() {
        let dir = tempfile::tempdir().unwrap();
        let result = command(dir.path(), "printf 'one\\ntwo\\n'; exit 1", None, 1);
        assert!(!result.passed);
        assert!(result.detail.contains("two"), "{}", result.detail);
        let ok = command(dir.path(), "true", None, 1);
        assert!(ok.passed);
    }

    #[test]
    fn a_simple_pdf_counts_pages() {
        let dir = tempfile::tempdir().unwrap();
        let pdf = b"%PDF-1.4\n1 0 obj << /Type /Pages /Count 2 >> endobj\n%%EOF";
        fs::write(dir.path().join("a.pdf"), pdf).unwrap();
        let result = pdf_pages(dir.path(), "a.pdf", Some(1), None);
        assert!(!result.passed);
        assert!(result.detail.contains("has 2 pages"));
        assert!(!pdf_pages(dir.path(), "missing.pdf", None, None).passed);
    }

    #[test]
    fn the_last_json_verdict_wins() {
        let text = "draft\n{\"pass\": false}\nfinal: {\"pass\": true, \"reasons\": [\"ok\"]}";
        let verdict = review_verdict(text).unwrap();
        assert!(verdict.pass);
        assert_eq!(verdict.reasons, vec!["ok"]);
        assert!(review_verdict("no verdict").is_none());
    }
}
