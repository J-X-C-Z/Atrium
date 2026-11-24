//! A step's prompt as the agent gets it: the run's shared rules, the
//! step's own instructions with the inputs filled in, the files it must
//! leave, how it will be checked, and what to fix from the last try.

use crate::{InputKind, check::describe, run::Run};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    Zh,
    En,
}

/// `{{input.<key>}}` filled from the run's inputs. Private inputs are
/// never filled in; their placeholder stays empty.
pub fn fill(text: &str, run: &Run) -> String {
    let mut out = text.to_string();
    for input in &run.template.inputs {
        let value = if input.private {
            String::new()
        } else {
            let raw = run.inputs.get(&input.key).cloned().unwrap_or_default();
            match input.kind {
                InputKind::Files => raw
                    .lines()
                    .filter(|l| !l.trim().is_empty())
                    .map(|l| format!("`{}`", l.trim()))
                    .collect::<Vec<_>>()
                    .join("、"),
                _ => raw,
            }
        };
        out = out.replace(&format!("{{{{input.{}}}}}", input.key), &value);
    }
    out
}

/// `text` cut to `max` characters, with an ellipsis when it was.
fn shorten(text: &str, max: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max {
        return trimmed.to_string();
    }
    let mut cut: String = trimmed.chars().take(max).collect();
    cut.push('…');
    cut
}

/// The full prompt for step `id`'s next try. `skills` says whether the
/// step's agent can use installed skills (Claude Code, `claude`, can).
pub fn prompt(run: &Run, id: &str, skills: bool, lang: Lang) -> Option<String> {
    let state = run.step(id)?;
    let step = run.spec(id)?;
    let zh = lang == Lang::Zh;
    let mut parts = Vec::new();
    if !run.template.preamble.trim().is_empty() {
        parts.push(fill(run.template.preamble.trim(), run));
    }
    if let Some(lessons) = run.experience.as_deref().filter(|e| !e.trim().is_empty()) {
        parts.push(if zh {
            format!(
                "### 以往比赛中你的队伍认可的经验（参考，不必照搬）\n{}",
                lessons.trim()
            )
        } else {
            format!(
                "### Lessons the team kept from earlier runs (for reference)\n{}",
                lessons.trim()
            )
        });
    }
    let ix = run.template.steps.iter().position(|s| s.id == step.id)?;
    let (n, total) = (ix + 1, run.template.steps.len());
    let title = run.title_of(id);
    parts.push(if zh {
        format!("## 当前步骤（{n}/{total}）：{title}")
    } else {
        format!("## Step {n} of {total}: {title}")
    });
    let decisions = run.decisions_before(id);
    if !decisions.is_empty() {
        let list = decisions
            .iter()
            .map(|d| {
                let at = run.title_of(&d.step);
                let mut line = match (&d.choice, zh) {
                    (Some(c), true) => format!("- 「{at}」：选定 {}", c.title),
                    (Some(c), false) => format!("- {at}: chose {}", c.title),
                    (None, true) => format!("- 「{at}」"),
                    (None, false) => format!("- {at}"),
                };
                if let Some(c) = &d.choice
                    && !c.summary.is_empty()
                {
                    line.push_str(&format!("（{}）", c.summary));
                }
                if !d.note.is_empty() {
                    line.push_str(&if zh {
                        format!("；补充：{}", d.note)
                    } else {
                        format!("; note: {}", d.note)
                    });
                }
                line
            })
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(if zh {
            format!("### 用户已经做出的决定（必须遵循；确实行不通时在报告里说明，不要擅自改变）\n{list}")
        } else {
            format!("### What the user decided (follow it; if it truly cannot work, say so in your report rather than change it)\n{list}")
        });
    }
    // Every try is a new conversation, so later steps learn what earlier
    // steps reported only from files and this section.
    let reports = run.answers_before(id);
    if !reports.is_empty() {
        let list = reports
            .iter()
            .map(|(title, answer)| format!("- {title}: {}", shorten(answer, 600)))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(if zh {
            format!("### 前面步骤的报告（供参考）\n{list}")
        } else {
            format!("### What earlier steps reported (for reference)\n{list}")
        });
    }
    if skills && let Some(skill) = &step.skill {
        parts.push(if zh {
            format!("如果你能使用 skill `/{skill}`，先调用它，并遵守其中的要求；与下面的要求冲突时，以下面为准。")
        } else {
            format!("If you can use the skill `/{skill}`, call it first and follow it; where it disagrees with what follows, follow what follows.")
        });
    }
    parts.push(run.fill_item(id, &fill(step.prompt.trim(), run)));
    if let Some(decide) = &step.decide {
        let file = run.fill_item(id, &decide.options);
        parts.push(if zh {
            format!(
                "### 这一步要请用户选择\n{}\n把候选方案写成 JSON 数组存到 `{file}`，每项包含 `id`、`title`（一句话的方案名）和 `summary`（优点、局限和适用前提）。给 2–4 个真正不同的方案，并在你的报告里写明你推荐哪个、为什么。不要替用户做决定。",
                decide.question
            )
        } else {
            format!(
                "### The user picks here\n{}\nWrite the candidates as a JSON array to `{file}`, each with `id`, `title` (the option in a line) and `summary` (strengths, limits, assumptions). Give 2–4 genuinely different options and say in your report which you recommend and why. Do not decide for the user.",
                decide.question
            )
        });
    }
    let outputs = run.outputs_of(id);
    if !outputs.is_empty() {
        let list = outputs
            .iter()
            .map(|o| format!("- `{o}`"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(if zh {
            format!("### 必须产出（路径相对于当前文件夹）\n{list}")
        } else {
            format!("### Leave these files (relative to this folder)\n{list}")
        });
    }
    let checks = run.checks_of(id);
    if !checks.is_empty() {
        let list = checks
            .iter()
            .map(|c| format!("- {}", describe(c, lang)))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(if zh {
            format!("### 完成后会检查\n{list}")
        } else {
            format!("### When you finish, this is checked\n{list}")
        });
    }
    if !step.fixes.is_empty() {
        let names = step
            .fixes
            .iter()
            .map(|f| format!("`{f}`"))
            .collect::<Vec<_>>()
            .join(if zh { "、" } else { ", " });
        parts.push(if zh {
            format!("### 发现问题时\n问题出在哪一步，就把它退回那一步修改。可以退回的步骤：{names}。在报告里写明每个问题属于哪一步。")
        } else {
            format!("### When you find faults\nSay which step each fault belongs to; it goes back there. Steps that can be sent back: {names}.")
        });
    }
    let feedback = if state.status == crate::run::StepStatus::Running {
        state.last().map(|a| a.feedback.clone()).unwrap_or_default()
    } else {
        state.feedback.clone()
    };
    if !feedback.is_empty() {
        let list = feedback
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n");
        parts.push(if zh {
            format!("### 这一步之前没有通过，请先修正这些问题（只改有问题的部分，别推倒重来）\n{list}")
        } else {
            format!("### This step fell short before; fix these first (change only what is wrong)\n{list}")
        });
    }
    parts.push(if zh {
        "做完后用一两句话说明你做了什么、产出在哪里。不要问我问题：拿不准的地方按最合理的方式处理，并在报告里写明。".into()
    } else {
        "When done, say in a sentence or two what you did and where it is. Do not ask me questions: decide the unclear points as best you can and note them in your report.".into()
    });
    Some(parts.join("\n\n"))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{Template, run::CheckResult};

    #[test]
    fn a_prompt_carries_inputs_outputs_checks_and_the_reasons_to_fix() {
        let t = Template::parse(
            r#"
id = "t"
name = "T"
preamble = "比赛：{{input.contest}}。"
[[inputs]]
key = "contest"
label = "比赛"
kind = "choice"
options = ["CUMCM", "MCM"]
[[inputs]]
key = "problem"
label = "题目"
kind = "files"
[[inputs]]
key = "team"
label = "队员姓名"
kind = "long"
private = true
[[steps]]
id = "write"
title = "撰写论文"
skill = "suite:5writing"
prompt = "根据 {{input.problem}} 写论文。不要写 {{input.team}}。"
outputs = ["paper/main.pdf"]
checks = [{ kind = "pdf_pages", path = "paper/main.pdf", max = 30 }]
"#,
        )
        .unwrap();
        let inputs: HashMap<String, String> = [
            (
                "problem".to_string(),
                "inputs/A.pdf\ninputs/data.xlsx".to_string(),
            ),
            ("team".to_string(), "张三".to_string()),
        ]
        .into();
        let mut run = Run::new(t, inputs, &[], 0).unwrap();
        let p = prompt(&run, "write", true, Lang::Zh).unwrap();
        assert!(p.starts_with("比赛：CUMCM。"));
        assert!(p.contains("`inputs/A.pdf`、`inputs/data.xlsx`"));
        assert!(!p.contains("张三"), "private inputs stay out of prompts");
        assert!(p.contains("/suite:5writing"));
        assert!(p.contains("- `paper/main.pdf`"));
        assert!(p.contains("paper/main.pdf 不超过 30 页"));
        assert!(
            !prompt(&run, "write", false, Lang::Zh)
                .unwrap()
                .contains("skill")
        );

        run.begin("write", None, 1).unwrap();
        run.turn_finished("write", 2).unwrap();
        run.checked(
            "write",
            vec![CheckResult::fail("paper/main.pdf 不超过 30 页", "有 33 页")],
            3,
        )
        .unwrap();
        let again = prompt(&run, "write", true, Lang::Zh).unwrap();
        assert!(again.contains("别推倒重来）\n- paper/main.pdf 不超过 30 页: 有 33 页"));
    }

    #[test]
    fn a_later_prompt_carries_what_earlier_steps_reported() {
        let t = Template::parse(
            r#"
id = "t"
name = "T"
[[steps]]
id = "plan"
title = "Plan"
prompt = "Plan."
[[steps]]
id = "write"
title = "Write"
prompt = "Write."
"#,
        )
        .unwrap();
        let mut run = Run::new(t, HashMap::new(), &[], 0).unwrap();
        run.begin("plan", None, 1).unwrap();
        run.turn_finished("plan", 2).unwrap();
        run.checked("plan", vec![CheckResult::pass("ok")], 3)
            .unwrap();
        run.set_answer("plan", "Use a queueing model.").unwrap();
        let p = prompt(&run, "write", false, Lang::En).unwrap();
        assert!(p.contains("What earlier steps reported"));
        assert!(p.contains("Plan: Use a queueing model."));

        let long = "x".repeat(700);
        run.set_answer("plan", long.clone()).unwrap();
        let p = prompt(&run, "write", false, Lang::En).unwrap();
        assert!(p.contains(&format!("Plan: {}…", "x".repeat(600))));
        assert!(!p.contains(&long), "a long report is shortened");
    }
}
