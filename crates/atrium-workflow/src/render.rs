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

/// The full prompt for step `id`'s next try. `skills` says whether the
/// step's agent can use installed skills (Claude Code can).
pub fn prompt(run: &Run, id: &str, skills: bool, lang: Lang) -> Option<String> {
    let ix = run.template.steps.iter().position(|s| s.id == id)?;
    let step = &run.template.steps[ix];
    let state = &run.steps[ix];
    let zh = lang == Lang::Zh;
    let mut parts = Vec::new();
    if !run.template.preamble.trim().is_empty() {
        parts.push(fill(run.template.preamble.trim(), run));
    }
    let (done, total) = (ix + 1, run.template.steps.len());
    parts.push(if zh {
        format!("## 当前步骤（{done}/{total}）：{}", step.title)
    } else {
        format!("## Step {done} of {total}: {}", step.title)
    });
    if skills && let Some(skill) = &step.skill {
        parts.push(if zh {
            format!("如果你能使用 skill `/{skill}`，先调用它，并遵守其中的要求；与下面的要求冲突时，以下面为准。")
        } else {
            format!("If you can use the skill `/{skill}`, call it first and follow it; where it disagrees with what follows, follow what follows.")
        });
    }
    parts.push(fill(step.prompt.trim(), run));
    if !step.outputs.is_empty() {
        let list = step
            .outputs
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
            format!("### 这一步之前没有通过，请先修正这些问题\n{list}")
        } else {
            format!("### This step fell short before; fix these first\n{list}")
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
            vec![CheckResult {
                what: "paper/main.pdf 不超过 30 页".into(),
                passed: false,
                detail: "有 33 页".into(),
            }],
            3,
        )
        .unwrap();
        let again = prompt(&run, "write", true, Lang::Zh).unwrap();
        assert!(again.contains("请先修正这些问题\n- paper/main.pdf 不超过 30 页: 有 33 页"));
    }
}
