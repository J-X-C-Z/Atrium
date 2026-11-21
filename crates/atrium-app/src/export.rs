//! A conversation as Markdown, to save or paste elsewhere. Thinking is left
//! out; commands keep their output, cut when long.

use atrium_core::{ChangeKind, EntryKind, Item, ItemStatus, Transcript};

use crate::i18n::{t, tf};

/// Lines of a command's output kept in the export.
const OUTPUT_LINES: usize = 40;

/// A fence longer than any run of backticks in `text`.
fn fence(text: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    "`".repeat((longest + 1).max(3))
}

fn block(lang: &str, text: &str) -> String {
    let f = fence(text);
    format!("{f}{lang}\n{}\n{f}\n", text.trim_end())
}

fn cut(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    if lines.len() <= OUTPUT_LINES {
        return output.to_string();
    }
    let mut kept = lines[..OUTPUT_LINES].join("\n");
    kept.push('\n');
    kept.push_str(&tf(
        "… {n} more lines",
        &[("n", &(lines.len() - OUTPUT_LINES))],
    ));
    kept
}

fn status_word(status: ItemStatus) -> &'static str {
    match status {
        ItemStatus::InProgress => t("running"),
        ItemStatus::Completed => "",
        ItemStatus::Failed => t("failed"),
        ItemStatus::Declined => t("declined"),
    }
}

/// `title` and every turn of `transcript`, as Markdown.
pub fn to_markdown(title: &str, folder: &str, transcript: &Transcript) -> String {
    let mut out = format!("# {}\n\n", title.trim());
    if !folder.is_empty() {
        out.push_str(&format!(
            "> {}\n\n",
            tf("Folder: {folder}", &[("folder", &folder)])
        ));
    }
    for entry in transcript.entries() {
        let part = match &entry.kind {
            EntryKind::Item(Item::User {
                text, attachments, ..
            }) => {
                let mut s = format!("## {}\n\n{}\n", t("You"), text.trim());
                for path in attachments {
                    s.push_str(&format!("\n- 📎 `{path}`"));
                }
                if !attachments.is_empty() {
                    s.push('\n');
                }
                s
            }
            EntryKind::Item(Item::Agent { text, .. }) if !text.trim().is_empty() => {
                format!("### {}\n\n{}\n", t("Agent"), text.trim())
            }
            EntryKind::Item(Item::Command {
                command,
                output,
                exit_code,
                status,
                ..
            }) => {
                let mut s = format!("**{}** {}\n\n", t("Ran"), status_word(*status));
                s.push_str(&block("sh", command));
                if !output.trim().is_empty() {
                    s.push('\n');
                    s.push_str(&block("", &cut(output)));
                }
                if let Some(code) = exit_code.filter(|c| *c != 0) {
                    s.push_str(&format!(
                        "\n{}\n",
                        tf("Exit code {code}", &[("code", &code)])
                    ));
                }
                s
            }
            EntryKind::Item(Item::FileChange { changes, .. }) => {
                let mut s = format!("**{}**\n\n", t("Changed files"));
                for c in changes {
                    let what = match c.kind {
                        ChangeKind::Add => t("new"),
                        ChangeKind::Delete => t("deleted"),
                        ChangeKind::Update { .. } => t("edited"),
                    };
                    s.push_str(&format!("- `{}` ({what})\n", c.path));
                }
                s
            }
            EntryKind::Item(Item::Tool { name, status, .. }) => {
                format!("**{}** `{name}` {}\n", t("Tool"), status_word(*status))
            }
            EntryKind::Item(Item::WebSearch { query, .. }) => {
                format!("**{}** {query}\n", t("Searched the web"))
            }
            EntryKind::Item(Item::Image {
                path: Some(path), ..
            }) => format!("![]({path})\n"),
            EntryKind::Plan { explanation, steps } => {
                let mut s = format!("**{}**\n\n", t("Plan"));
                if let Some(e) = explanation {
                    s.push_str(&format!("{}\n\n", e.trim()));
                }
                for step in steps {
                    s.push_str(&format!(
                        "- [{}] {}\n",
                        if step.done { "x" } else { " " },
                        step.step
                    ));
                }
                s
            }
            EntryKind::Question {
                request, answers, ..
            } => {
                let mut s = String::new();
                for q in &request.questions {
                    s.push_str(&format!("**{}** {}\n", t("Question"), q.question));
                    if let Some(given) = answers
                        .as_ref()
                        .and_then(|a| a.iter().find(|(id, _)| *id == q.id))
                    {
                        s.push_str(&format!("\n> {}\n", given.1.join(", ")));
                    }
                }
                s
            }
            EntryKind::Error(text) => format!("> ⚠️ {}\n", text.trim()),
            _ => continue,
        };
        out.push_str(&part);
        out.push('\n');
    }
    out.push_str(&format!(
        "---\n\n{}\n",
        tf(
            "Exported from Atrium on {date}",
            &[("date", &jiff::Zoned::now().strftime("%Y-%m-%d %H:%M"))]
        )
    ));
    out
}

/// A file name for the export: the title, without characters file systems refuse.
pub fn file_name(title: &str) -> String {
    let name: String = title
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                ' '
            } else {
                c
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let name: String = name.chars().take(60).collect();
    format!(
        "{}.md",
        if name.is_empty() {
            "Atrium"
        } else {
            name.as_str()
        }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_outlast_the_text() {
        assert_eq!(fence("plain"), "```");
        assert_eq!(fence("has ``` inside"), "````");
    }

    #[test]
    fn long_output_is_cut() {
        let long = (0..100)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let kept = cut(&long);
        assert!(kept.lines().count() == OUTPUT_LINES + 1);
        assert!(kept.ends_with("60 more lines"));
    }

    #[test]
    fn a_conversation_reads_as_markdown() {
        let mut tr = Transcript::new();
        tr.push_local_user("修一下登录页", vec![]);
        let md = to_markdown("登录页", "/code/app", &tr);
        assert!(md.starts_with("# 登录页\n"));
        assert!(md.contains("/code/app"));
        assert!(md.contains("修一下登录页"));
    }

    #[test]
    fn file_names_are_safe() {
        assert_eq!(file_name("Fix: a/b?"), "Fix a b.md");
        assert_eq!(file_name("  "), "Atrium.md");
    }
}
