//! The checks that only read the run folder: files exist, hold or do not
//! hold something. Commands, page counts and reviews are the runner's.

use std::path::{Path, PathBuf};

use regex::Regex;

use crate::{
    Check,
    render::Lang,
    run::{CheckResult, Run},
};

/// A verdict is small; reading more than this is a mistake, not a verdict.
const VERDICT_BYTE_CAP: usize = 1024 * 1024;

/// Files under `dir` matching `pattern` (a path or a glob, relative). A
/// match that resolves outside the run folder, through `..` or a symlink,
/// is left out.
pub fn matches(dir: &Path, pattern: &str) -> Vec<PathBuf> {
    let full = dir.join(pattern);
    let Some(text) = full.to_str() else {
        return Vec::new();
    };
    glob::glob(text)
        .map(|paths| {
            paths
                .filter_map(Result::ok)
                .filter(|path| crate::files::is_contained(dir, path))
                .collect()
        })
        .unwrap_or_default()
}

/// Not empty: a file with bytes, or a folder with a file somewhere in it.
fn filled(path: &Path) -> bool {
    if path.is_file() {
        return path.metadata().is_ok_and(|m| m.len() > 0);
    }
    std::fs::read_dir(path)
        .map(|entries| entries.filter_map(Result::ok).any(|e| filled(&e.path())))
        .unwrap_or(false)
}

/// Text files matched by `pattern`, folders walked; others are left out.
fn texts(dir: &Path, pattern: &str) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    let mut stack = matches(dir, pattern);
    while let Some(path) = stack.pop() {
        if !crate::files::is_contained(dir, &path) {
            continue;
        }
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                stack.extend(
                    entries
                        .filter_map(Result::ok)
                        .map(|e| e.path())
                        .filter(|path| crate::files::is_contained(dir, path)),
                );
            }
        } else if let Ok(bytes) = std::fs::read(&path)
            && bytes.len() <= 8 * 1024 * 1024
            && let Ok(text) = String::from_utf8(bytes)
        {
            out.push((path, text));
        }
    }
    out.sort();
    out
}

fn rel(dir: &Path, path: &Path) -> String {
    path.strip_prefix(dir)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// The check in words, as the user and the agent read it.
pub fn describe(check: &Check, lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    match check {
        Check::Exists { path, min } => match (min, zh) {
            (Some(n), true) => format!("{path} 至少有 {n} 个文件"),
            (Some(n), false) => format!("at least {n} files match {path}"),
            (None, true) => format!("{path} 存在且不为空"),
            (None, false) => format!("{path} exists and is not empty"),
        },
        Check::Contains {
            path,
            pattern,
            message,
        } => message.clone().unwrap_or_else(|| {
            if zh {
                format!("{path} 包含 /{pattern}/")
            } else {
                format!("{path} contains /{pattern}/")
            }
        }),
        Check::Absent { path, message, .. } => message.clone().unwrap_or_else(|| {
            if zh {
                format!("{path} 中没有不该出现的内容")
            } else {
                format!("{path} holds nothing it must not")
            }
        }),
        Check::Command { run, message, .. } => message.clone().unwrap_or_else(|| {
            if zh {
                format!("命令 `{run}` 运行成功")
            } else {
                format!("`{run}` succeeds")
            }
        }),
        Check::PdfPages { path, max, min } => match (min, max, zh) {
            (Some(a), Some(b), true) => format!("{path} 在 {a}–{b} 页之间"),
            (None, Some(b), true) => format!("{path} 不超过 {b} 页"),
            (Some(a), None, true) => format!("{path} 至少 {a} 页"),
            (Some(a), Some(b), false) => format!("{path} has {a}–{b} pages"),
            (None, Some(b), false) => format!("{path} has at most {b} pages"),
            (Some(a), None, false) => format!("{path} has at least {a} pages"),
            (None, None, true) => format!("{path} 是有效的 PDF"),
            (None, None, false) => format!("{path} is a valid PDF"),
        },
        Check::Verdict { path } => {
            if zh {
                format!("{path} 的结论为通过")
            } else {
                format!("{path} says it passes")
            }
        }
        Check::Review { rubric, .. } => {
            let first = rubric.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            if zh {
                format!("审稿通过：{}", first.trim())
            } else {
                format!("A reviewer passes it: {}", first.trim())
            }
        }
    }
}

/// What a verdict file says: passed, or each issue with the step at fault.
pub fn read_verdict(text: &str, lang: Lang) -> Vec<CheckResult> {
    let zh = lang == Lang::Zh;
    let Ok(v) = serde_json::from_str::<serde_json::Value>(text) else {
        return vec![CheckResult::fail(
            if zh { "验收结论" } else { "the verdict" },
            if zh {
                "不是有效的 JSON"
            } else {
                "not valid JSON"
            },
        )];
    };
    let issues = v["issues"].as_array().cloned().unwrap_or_default();
    let mut out: Vec<CheckResult> = issues
        .iter()
        .filter(|i| i["blocking"].as_bool() != Some(false))
        .map(|i| {
            let s = |k: &str| i[k].as_str().unwrap_or("").trim().to_string();
            let mut r = CheckResult::fail(
                if s("problem").is_empty() {
                    s("title")
                } else {
                    s("problem")
                },
                s("fix"),
            );
            r.send_back = Some(s("step")).filter(|x| !x.is_empty());
            r
        })
        .collect();
    let passed = v["pass"].as_bool().unwrap_or(false);
    if out.is_empty() {
        out.push(if passed {
            CheckResult::pass(if zh {
                "验收结论为通过"
            } else {
                "the verdict passes"
            })
        } else {
            CheckResult::fail(
                if zh {
                    "验收结论为未通过"
                } else {
                    "the verdict does not pass"
                },
                if zh {
                    "没有列出问题"
                } else {
                    "no issues listed"
                },
            )
        });
    }
    out
}

/// Runs a check that only reads files; `None` for the runner's kinds.
/// `private` gives a private input's values, one per line.
pub fn run_local(
    dir: &Path,
    check: &Check,
    private: &dyn Fn(&str) -> Option<String>,
    lang: Lang,
) -> Option<CheckResult> {
    let zh = lang == Lang::Zh;
    let what = describe(check, lang);
    let result = |passed: bool, detail: String| CheckResult {
        what: what.clone(),
        passed,
        detail,
        send_back: None,
    };
    Some(match check {
        Check::Exists { path, min } => {
            let found: Vec<PathBuf> = matches(dir, path)
                .into_iter()
                .filter(|p| filled(p))
                .collect();
            let need = min.unwrap_or(1).max(1);
            if found.len() >= need {
                result(true, String::new())
            } else if found.is_empty() {
                result(
                    false,
                    if zh {
                        "没有找到，或是空的".into()
                    } else {
                        "not found, or empty".into()
                    },
                )
            } else {
                result(
                    false,
                    if zh {
                        format!("只有 {} 个", found.len())
                    } else {
                        format!("only {}", found.len())
                    },
                )
            }
        }
        Check::Contains { path, pattern, .. } => {
            let re = Regex::new(pattern).ok()?;
            let files = texts(dir, path);
            if files.is_empty() {
                result(
                    false,
                    if zh {
                        "文件不存在".into()
                    } else {
                        "no such file".into()
                    },
                )
            } else if files.iter().any(|(_, t)| re.is_match(t)) {
                result(true, String::new())
            } else {
                result(
                    false,
                    if zh {
                        "没有找到".into()
                    } else {
                        "not found".into()
                    },
                )
            }
        }
        Check::Absent {
            path,
            pattern,
            input,
            ..
        } => {
            let re = pattern.as_deref().and_then(|p| Regex::new(p).ok());
            let words: Vec<String> = input
                .as_deref()
                .and_then(private)
                .map(|v| {
                    v.lines()
                        .map(str::trim)
                        .filter(|w| w.chars().count() >= 2)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let mut hits = Vec::new();
            for (file, text) in texts(dir, path) {
                for (n, line) in text.lines().enumerate() {
                    let bad = re
                        .as_ref()
                        .and_then(|re| re.find(line))
                        .map(|m| m.as_str().to_string());
                    // Private values are never echoed back, only where they are.
                    let private_hit = words.iter().any(|w| line.contains(w.as_str()));
                    if bad.is_some() || private_hit {
                        let shown = match (bad, private_hit) {
                            (Some(m), _) => format!("「{m}」"),
                            (None, _) => {
                                if zh {
                                    "身份信息".into()
                                } else {
                                    "identifying details".into()
                                }
                            }
                        };
                        hits.push(format!("{}:{} {shown}", rel(dir, &file), n + 1));
                        if hits.len() >= 10 {
                            break;
                        }
                    }
                }
            }
            if hits.is_empty() {
                result(true, String::new())
            } else {
                result(false, hits.join("; "))
            }
        }
        Check::Verdict { path } => {
            let Ok(text) = crate::files::read_text(dir, path, VERDICT_BYTE_CAP) else {
                return Some(result(
                    false,
                    if zh {
                        "文件不存在".into()
                    } else {
                        "no such file".into()
                    },
                ));
            };
            // Several results: the caller takes them through `run_local_all`.
            let results = read_verdict(&text, lang);
            let failed: Vec<&CheckResult> = results.iter().filter(|r| !r.passed).collect();
            if failed.is_empty() {
                result(true, String::new())
            } else {
                result(
                    false,
                    failed
                        .iter()
                        .map(|r| r.what.clone())
                        .collect::<Vec<_>>()
                        .join("; "),
                )
            }
        }
        _ => return None,
    })
}

/// The read-only checks step `id` implies for `run`: each output exists,
/// then its own file checks. The runner adds the results of the others,
/// in [`Run::checks_of`] order.
pub fn local_results(dir: &Path, run: &Run, id: &str, lang: Lang) -> Vec<CheckResult> {
    let private = |key: &str| {
        run.template
            .inputs
            .iter()
            .any(|i| i.key == key && i.private)
            .then(|| run.inputs.get(key).cloned())
            .flatten()
    };
    let mut out = Vec::new();
    for output in run.outputs_of(id) {
        let check = Check::Exists {
            path: output,
            min: None,
        };
        out.extend(run_local(dir, &check, &private, lang));
    }
    for check in run.checks_of(id) {
        if let Check::Verdict { path } = &check
            && let Ok(text) = crate::files::read_text(dir, path, VERDICT_BYTE_CAP)
        {
            out.extend(read_verdict(&text, lang));
            continue;
        }
        out.extend(run_local(dir, &check, &private, lang));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_are_checked_for_being_there_and_what_they_hold() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join("figures")).unwrap();
        std::fs::write(d.join("figures/a.pdf"), b"%PDF").unwrap();
        std::fs::write(d.join("empty.md"), b"").unwrap();
        std::fs::write(
            d.join("paper.tex"),
            "\\section{摘要}\n本文由张三完成。\nTODO: 补充\n",
        )
        .unwrap();
        let none = |_: &str| None;
        let lang = Lang::Zh;
        let exists = |p: &str, min| {
            run_local(
                d,
                &Check::Exists {
                    path: p.into(),
                    min,
                },
                &none,
                lang,
            )
            .unwrap()
        };
        assert!(exists("figures", None).passed);
        assert!(exists("figures/*.pdf", None).passed);
        assert!(!exists("figures/*.pdf", Some(2)).passed);
        assert!(!exists("empty.md", None).passed);
        assert!(!exists("missing.md", None).passed);

        let contains = Check::Contains {
            path: "paper.tex".into(),
            pattern: "摘要".into(),
            message: None,
        };
        assert!(run_local(d, &contains, &none, lang).unwrap().passed);

        let names = |key: &str| (key == "team").then(|| "张三\n李四".to_string());
        let absent = Check::Absent {
            path: "*.tex".into(),
            pattern: Some("TODO|待补充".into()),
            input: Some("team".into()),
            message: None,
        };
        let r = run_local(d, &absent, &names, lang).unwrap();
        assert!(!r.passed);
        assert!(r.detail.contains("paper.tex:2 身份信息"), "{}", r.detail);
        assert!(r.detail.contains("paper.tex:3 「TODO」"), "{}", r.detail);
        assert!(
            !r.detail.contains("张三"),
            "private values are never echoed"
        );
    }

    #[test]
    fn a_verdict_names_the_step_at_fault() {
        let r = read_verdict(
            r#"{"pass": false, "issues": [
                {"step": "solve:Q2", "problem": "Q2 的目标值与论文不一致", "fix": "以 results/Q2 为准"},
                {"step": "writing", "problem": "摘要超过一页", "fix": "压缩问题重述", "blocking": true},
                {"step": "writing", "problem": "图 3 配色", "blocking": false}
            ]}"#,
            Lang::Zh,
        );
        assert_eq!(r.len(), 2, "advice that does not block is left out");
        assert_eq!(r[0].send_back.as_deref(), Some("solve:Q2"));
        assert!(!r[0].passed);
        assert!(read_verdict(r#"{"pass": true, "issues": []}"#, Lang::Zh)[0].passed);
        assert!(!read_verdict("not json", Lang::Zh)[0].passed);
    }

    #[test]
    fn checks_never_read_outside_the_run_folder() {
        let root = tempfile::tempdir().unwrap();
        let run = root.path().join("run");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(root.path().join("outside.txt"), "secret").unwrap();
        std::fs::write(root.path().join("verdict.json"), r#"{"pass": true}"#).unwrap();
        let none = |_: &str| None;
        let contains = Check::Contains {
            path: "../outside.txt".into(),
            pattern: "secret".into(),
            message: None,
        };
        let result = run_local(&run, &contains, &none, Lang::En).unwrap();
        assert!(!result.passed, "a parent path is not part of the run");
        let verdict = Check::Verdict {
            path: "../verdict.json".into(),
        };
        assert!(!run_local(&run, &verdict, &none, Lang::En).unwrap().passed);
        assert!(matches(&run, "../outside.txt").is_empty());

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.path().join("outside.txt"), run.join("link.txt"))
                .unwrap();
            let exists = Check::Exists {
                path: "link.txt".into(),
                min: None,
            };
            assert!(
                !run_local(&run, &exists, &none, Lang::En).unwrap().passed,
                "a symlink out of the run folder is left out"
            );
            std::fs::create_dir_all(run.join("figures")).unwrap();
            std::os::unix::fs::symlink(
                root.path().join("outside.txt"),
                run.join("figures/link.txt"),
            )
            .unwrap();
            let nested = Check::Contains {
                path: "figures".into(),
                pattern: "secret".into(),
                message: None,
            };
            assert!(
                !run_local(&run, &nested, &none, Lang::En).unwrap().passed,
                "a folder walk does not follow a symlink out"
            );
        }
    }

    #[test]
    fn runner_kinds_are_left_to_the_runner() {
        let dir = tempfile::tempdir().unwrap();
        let cmd = Check::Command {
            run: "true".into(),
            cwd: None,
            timeout_min: 1,
            message: None,
        };
        assert!(run_local(dir.path(), &cmd, &|_| None, Lang::En).is_none());
    }
}
