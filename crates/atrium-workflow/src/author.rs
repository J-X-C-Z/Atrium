//! Writing templates with an agent's help: the format guide the agent
//! works from, the first message of the authoring chat, and what Atrium
//! tells the user (and the agent) about a draft: whether it reads, and the
//! things that read but will not work as meant.

use std::collections::HashSet;

use crate::{Check, InputKind, Template, TemplateError, render::Lang};

/// The file in a draft folder the agent writes the template to.
pub const DRAFT_FILE: &str = "template.toml";
/// The format guide, written next to it.
pub const GUIDE_FILE: &str = "FORMAT.md";
/// A full template to learn from.
pub const EXAMPLE_FILE: &str = "example.toml";
/// Files the user gave as references.
pub const REFERENCES_DIR: &str = "references";

/// The format, for the agent: every field, what Atrium does with it, and
/// how to design a path that can be checked. Kept in step with the types
/// in this crate by `the_guide_names_every_field`.
pub const GUIDE: &str = r#"# Atrium workflow templates

A workflow is a preset path of steps that agents follow from what the user
gives at the start (a problem, data, a topic) to a finished result. Atrium
starts an agent on each step when the steps it needs are done, checks the
step's work, sends the reasons back for another try when it falls short,
and stops for the user where the template says so.

A template is one TOML file. Write it to `template.toml` in this folder.

## Top level

```toml
id = "lit-review"          # letters, digits, - and _ ; stable, used as the file name
name = "文献综述"           # shown to the user
description = "从主题到一篇带引用的综述"  # one or two lines
version = 1                # bump when steps change meaning
preamble = """
Rules every step's prompt starts with: shared conventions, where state is
kept, what must never be done.
"""

[defaults]
agent = "claude"           # used by steps that leave `agent` out
effort = "high"            # optional
attempts = 2               # tries per step before the run stops for the user
timeout_min = 90           # minutes one try may take
```

Agents: `codex` (Codex: good at code, running programs, data), `claude`
(Claude Code: good at analysis, writing, and can use installed skills),
`deepseek`, `antigravity`. Only use others if the user names them.

## Inputs: what the user gives at the start

```toml
[[inputs]]
key = "topic"              # used in prompts as {{input.topic}}
label = "主题"
help = "一句话说清楚要综述什么"   # placeholder / hint
kind = "text"              # files | text | long | choice | number
required = true

[[inputs]]
key = "venue"
label = "投稿方向"
kind = "choice"
options = ["期刊", "学位论文", "课程作业"]   # the first is the default
default = "期刊"           # optional

[[inputs]]
key = "authors"
label = "作者姓名"
kind = "long"
private = true             # never put into prompts; only `absent` checks read it
when = "venue=期刊"        # shown only when another input has this value (also `!=`)
```

`files` inputs are copied into the run's `inputs/` folder; in a prompt
`{{input.<key>}}` becomes the list of their paths.

## Steps

Steps run in the order written unless `needs` says otherwise. A step may
only need, or send back to, steps written before it.

```toml
[[steps]]
id = "search"
title = "检索文献"
summary = "one line under the title"
agent = "claude"           # optional, else defaults.agent
effort = "high"            # optional
skill = "some-plugin:some-skill"   # optional hint, used when installed
needs = ["scope"]          # optional; left out = the step before; [] = none
prompt = """
What to do, in the user's language. Refer to inputs as {{input.topic}}.
Say which files to read and which to write.
"""
outputs = ["refs/library.bib", "reports/search.md"]   # must exist and not be empty; globs allowed
gate = true                # stop for the user after it passes its checks
optional = true            # the user may leave it out at the start
when = "venue!=课程作业"     # only for some inputs
attempts = 3               # optional, else defaults.attempts
timeout_min = 120          # optional
```

### Checks: how Atrium tells the step did its job

Each failed check's reason goes back to the agent for the next try.

```toml
checks = [
  { kind = "exists",   path = "figures/*.png", min = 3 },
  { kind = "contains", path = "paper.tex", pattern = "\\\\begin\\{abstract\\}", message = "需要摘要" },
  { kind = "absent",   path = "paper.tex", pattern = "(?i)lorem ipsum" },
  { kind = "absent",   path = "paper.tex", input = "authors", message = "匿名稿不能出现作者" },
  { kind = "command",  run = "xelatex -interaction=nonstopmode paper.tex", cwd = "paper", timeout_min = 5 },
  { kind = "pdf_pages", path = "paper/paper.pdf", max = 25 },
  { kind = "verdict",  path = "reports/verify.json" },
  { kind = "review",   rubric = "论证完整、引用可查", files = ["paper.tex"], agent = "codex" },
  { kind = "exists",   path = "summary.pdf", when = "venue=期刊" },
]
```

- `pattern` is a regular expression (Rust syntax; escape backslashes in TOML).
- `command` runs in the run folder (or `cwd` under it) and must exit 0.
- `verdict`: the step itself writes a JSON verdict
  `{"pass": false, "issues": [{"step": "<step id>", "problem": "...", "fix": "...", "blocking": true}]}`.
  Each blocking issue fails the step; `step` says where the fault is.
- `review`: a separate agent reads `files` against `rubric` and must answer
  that it passes.

### Sending faults back: `fixes`

For a step that reviews earlier work. When its checks fail, the reasons go
to an earlier step to fix; that step and the ones after it run again.

```toml
fixes = "draft"                       # one step
fixes = ["writing", "solve", "data"]  # several: a verdict issue's `step` picks; otherwise the first
```

### Letting the user choose: `decide`

The step writes options; the run stops and the user picks one (with a
note). The choice goes into every later step's prompt.

```toml
decide = { options = "reports/routes.json", question = "选哪条路线？" }
outputs = ["reports/routes.json"]
```

The options file is a JSON list of `{"id", "title", "summary"}` (or plain
strings). Don't make the step decide for the user.

### Once per item: `each`

The step runs once per item that an earlier step listed, e.g. once per
sub-question; the copies can run at the same time, and later steps wait
for all of them.

```toml
each = { from = "reports/parts.json", max = 8 }
prompt = "处理 {{item.title}}，结果写到 results/{{item.id}}/。"
outputs = ["results/{{item.id}}/summary.md"]
```

The list file is a JSON list of `{"id", "title"}` (or plain strings), and
must be an output of an earlier step.

## Designing a good path

- One step, one job, with files to show for it. A step with no outputs and
  no checks cannot be checked.
- Keep state in files (notes, a state file), not in the conversation: each
  step starts a fresh chat and only sees files.
- Put the stops where a person's judgment matters: choosing a direction
  (`decide`), approving assumptions (`gate`), the final result.
- End with a step that checks the whole (`verdict` + `fixes`), so faults go
  back to where they started instead of piling up.
- Prefer checks that read files (`exists`, `contains`, `absent`, `verdict`)
  and real commands over vague rubrics.
- Write names, labels and prompts in the user's language.
"#;

/// The first message of an authoring chat. `base` is the template being
/// changed, when it is.
pub fn brief(request: &str, base: Option<&Template>, references: bool, lang: Lang) -> String {
    let zh = lang == Lang::Zh;
    let request = request.trim();
    // The request comes first: the chat is named after its opening words.
    let mut out = match (base, zh) {
        (Some(t), true) => format!(
            "修改工作流「{}」：{request}\n\n---\n当前版本在 `{DRAFT_FILE}`，改完写回这个文件。",
            t.name
        ),
        (Some(t), false) => format!(
            "Change the workflow \"{}\": {request}\n\n---\nIt is in `{DRAFT_FILE}`; write the new version back there.",
            t.name
        ),
        (None, true) => {
            format!("{request}\n\n---\n把上面的需求设计成 Atrium 工作流模板，写到 `{DRAFT_FILE}`。")
        }
        (None, false) => format!(
            "{request}\n\n---\nDesign the above as an Atrium workflow template and write it to `{DRAFT_FILE}`."
        ),
    };
    if zh {
        out.push_str(&format!(
            "格式见 `{GUIDE_FILE}`，`{EXAMPLE_FILE}` 是一个完整的例子。"
        ));
        if references {
            out.push_str(&format!(
                "`{REFERENCES_DIR}/` 里是我给的参考资料，先读一下。"
            ));
        }
        out.push_str(
            "\n\n要求：\n\
             - 关键的地方不清楚就先问我（最多三个问题）；不影响大局的自己定，写完列出你做的假设。\n\
             - 每一步都要留下能检查的文件，并写上检查方法；需要我判断的地方设成停下等我。\n\
             - 名称、说明和每步的指令用中文。\n\
             - 只写模板，不要真的去执行这个工作流。\n\
             - Atrium 会在右侧校验并显示步骤预览，我把校验问题发给你时照着改。",
        );
    } else {
        out.push_str(&format!(
            " The format is in `{GUIDE_FILE}`; `{EXAMPLE_FILE}` is a full example."
        ));
        if references {
            out.push_str(&format!(
                " `{REFERENCES_DIR}/` holds references I gave; read them first."
            ));
        }
        out.push_str(
            "\n\nPlease:\n\
             - Ask me first about what matters and is unclear (three questions at most); decide the rest and list your assumptions after.\n\
             - Give every step files that can be checked, and say how; stop for me where my judgment is needed.\n\
             - Only write the template; don't carry out the workflow itself.\n\
             - Atrium checks the file and shows a preview beside the chat; when I send you its findings, fix them.",
        );
    }
    out
}

/// A template error in the user's language.
pub fn explain(error: &TemplateError, lang: Lang) -> String {
    if lang == Lang::En {
        return error.to_string();
    }
    match error {
        TemplateError::Parse(why) => format!("文件读不出来：{why}"),
        TemplateError::NoSteps => "模板里没有步骤".into(),
        TemplateError::DuplicateStep(id) => format!("有两个步骤都叫 {id}"),
        TemplateError::DuplicateInput(key) => format!("有两个输入都叫 {key}"),
        TemplateError::UnknownNeed { step, missing } => {
            format!("步骤 {step} 依赖 {missing}，但它不是写在前面的步骤")
        }
        TemplateError::UnknownFix { step, target } => {
            format!("步骤 {step} 要退回 {target}，但它不是写在前面的步骤")
        }
        TemplateError::EmptyPrompt(id) => format!("步骤 {id} 没有写指令（prompt）"),
        TemplateError::BadId(id) => format!("{id} 不能当 id：只能用字母、数字、- 和 _"),
        TemplateError::BadCheck { step, why } => format!("步骤 {step} 的检查有误：{why}"),
        TemplateError::UnknownInput { what, key } => {
            format!("{what} 用到了 {key}，但没有这个输入")
        }
        TemplateError::NoOptions(key) => format!("选择题输入 {key} 没有选项"),
    }
}

/// What Atrium found in a draft.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    pub template: Option<Template>,
    /// Why it does not read, when it does not.
    pub error: Option<String>,
    /// Things that read but will not work as meant.
    pub warnings: Vec<String>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.template.is_some() && self.warnings.is_empty()
    }

    /// The findings as a message to the agent; `None` when there are none.
    pub fn as_feedback(&self, lang: Lang) -> Option<String> {
        let zh = lang == Lang::Zh;
        let mut lines = Vec::new();
        if let Some(error) = &self.error {
            lines.push(format!("- {error}"));
        }
        lines.extend(self.warnings.iter().map(|w| format!("- {w}")));
        if lines.is_empty() {
            return None;
        }
        let head = if zh {
            format!("Atrium 校验 `{DRAFT_FILE}` 发现这些问题，请修改：")
        } else {
            format!("Atrium checked `{DRAFT_FILE}` and found these; please fix them:")
        };
        Some(format!("{head}\n{}", lines.join("\n")))
    }
}

/// Reads a draft and reports on it. `agents` are the agent ids this
/// Atrium knows.
pub fn inspect(text: &str, agents: &[&str], lang: Lang) -> Report {
    match Template::parse(text) {
        Ok(template) => {
            let warnings = lint(&template, agents, lang);
            Report {
                template: Some(template),
                error: None,
                warnings,
            }
        }
        Err(error) => Report {
            template: None,
            error: Some(explain(&error, lang)),
            warnings: Vec::new(),
        },
    }
}

/// `{{input.<key>}}` keys used in `text`.
fn input_refs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{input.") {
        rest = &rest[start + 8..];
        let Some(end) = rest.find("}}") else { break };
        out.push(rest[..end].trim().to_string());
        rest = &rest[end..];
    }
    out
}

/// What reads but will not work as meant.
pub fn lint(t: &Template, agents: &[&str], lang: Lang) -> Vec<String> {
    let zh = lang == Lang::Zh;
    let mut out = Vec::new();
    let unknown_agent = |what: &str, agent: &str| {
        if zh {
            format!(
                "{what} 用的 agent「{agent}」Atrium 不认识，可用的有：{}",
                agents.join("、")
            )
        } else {
            format!(
                "{what} uses agent \"{agent}\", which Atrium does not know; known: {}",
                agents.join(", ")
            )
        }
    };
    if let Some(agent) = &t.defaults.agent
        && !agents.contains(&agent.as_str())
    {
        out.push(unknown_agent("defaults", agent));
    }
    let keys: HashSet<&str> = t.inputs.iter().map(|i| i.key.as_str()).collect();
    let private: HashSet<&str> = t
        .inputs
        .iter()
        .filter(|i| i.private)
        .map(|i| i.key.as_str())
        .collect();
    let check_refs = |what: &str, text: &str, out: &mut Vec<String>| {
        for key in input_refs(text) {
            if !keys.contains(key.as_str()) {
                out.push(if zh {
                    format!("{what} 里的 {{{{input.{key}}}}} 没有对应的输入")
                } else {
                    format!("{what} uses {{{{input.{key}}}}}, which is not an input")
                });
            } else if private.contains(key.as_str()) {
                out.push(if zh {
                    format!("{what} 用了私密输入 {key}：私密输入不会填进指令，只能给 absent 检查用")
                } else {
                    format!(
                        "{what} uses the private input {key}: private inputs are never put into prompts, only absent checks read them"
                    )
                });
            }
        }
    };
    check_refs("preamble", &t.preamble, &mut out);
    for input in &t.inputs {
        if input.kind == InputKind::Choice
            && let Some(default) = &input.default
            && !input.options.contains(default)
        {
            out.push(if zh {
                format!("输入 {} 的默认值「{default}」不在选项里", input.key)
            } else {
                format!(
                    "input {}: the default \"{default}\" is not an option",
                    input.key
                )
            });
        }
    }
    let mut written: Vec<&str> = Vec::new();
    let mut stops = false;
    for step in &t.steps {
        let id = step.id.as_str();
        let name = if zh {
            format!("步骤 {id}")
        } else {
            format!("step {id}")
        };
        if let Some(agent) = &step.agent
            && !agents.contains(&agent.as_str())
        {
            out.push(unknown_agent(&name, agent));
        }
        check_refs(&name, &step.prompt, &mut out);
        let uses_item =
            step.prompt.contains("{{item.") || step.outputs.iter().any(|o| o.contains("{{item."));
        if uses_item && step.each.is_none() {
            out.push(if zh {
                format!("{name} 用了 {{{{item.…}}}}，但它没有 each，不会按条目展开")
            } else {
                format!("{name} uses {{{{item.…}}}} but has no `each`, so it never runs per item")
            });
        }
        if let Some(each) = &step.each
            && !written.contains(&each.from.as_str())
        {
            out.push(if zh {
                format!("{name} 按 {} 展开，但前面没有步骤把它列为输出", each.from)
            } else {
                format!(
                    "{name} runs per item of {}, which no earlier step lists as an output",
                    each.from
                )
            });
        }
        if let Some(decide) = &step.decide {
            stops = true;
            if !step.outputs.contains(&decide.options) {
                out.push(if zh {
                    format!("{name} 让你选的方案在 {}，把它也写进这一步的 outputs，才能检查它写出来了", decide.options)
                } else {
                    format!(
                        "{name} reads options from {}; list it in the step's outputs too so it is checked",
                        decide.options
                    )
                });
            }
        }
        stops |= step.gate;
        if step.outputs.is_empty() && step.checks.is_empty() {
            out.push(if zh {
                format!("{name} 没有输出文件也没有检查，Atrium 无法判断它做没做完")
            } else {
                format!("{name} has no outputs and no checks, so Atrium cannot tell it is done")
            });
        }
        if !step.fixes.is_empty()
            && !step
                .checks
                .iter()
                .any(|c| matches!(c.check, Check::Verdict { .. } | Check::Review { .. }))
        {
            out.push(if zh {
                format!("{name} 会退回前面的步骤，但没有 verdict 或 review 检查来指出问题在哪")
            } else {
                format!("{name} sends faults back but has no verdict or review check to find them")
            });
        }
        if let Some(when) = &step.when
            && !when.contains('=')
        {
            out.push(if zh {
                format!("{name} 的条件「{when}」要写成 key=value 或 key!=value")
            } else {
                format!("{name}: the condition \"{when}\" should read key=value or key!=value")
            });
        }
        written.extend(step.outputs.iter().map(String::as_str));
    }
    if !stops {
        out.push(if zh {
            "整个流程没有一处停下来等你（gate 或 decide），会一路跑到底".into()
        } else {
            "The run never stops for the user (no gate or decide); it goes straight to the end"
                .into()
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENTS: &[&str] = &["codex", "claude"];

    const DRAFT: &str = r#"
id = "essay"
name = "Essay"

[[inputs]]
key = "topic"
label = "Topic"
kind = "text"

[[inputs]]
key = "names"
label = "Names"
kind = "long"
private = true

[[steps]]
id = "parts"
title = "Parts"
prompt = "Split {{input.topic}} into parts."
outputs = ["parts.json"]
gate = true

[[steps]]
id = "write"
title = "Write"
agent = "claude"
each = { from = "parts.json" }
prompt = "Write {{item.title}}."
outputs = ["parts/{{item.id}}.md"]
"#;

    #[test]
    fn a_sound_draft_has_nothing_to_say() {
        let report = inspect(DRAFT, AGENTS, Lang::En);
        assert!(report.is_clean(), "{:?}", report.warnings);
        assert_eq!(report.as_feedback(Lang::Zh), None);
    }

    #[test]
    fn drafts_that_will_not_work_are_told_why() {
        let bad = DRAFT
            .replace("agent = \"claude\"", "agent = \"claude-code\"")
            .replace("from = \"parts.json\"", "from = \"list.json\"")
            .replace("gate = true", "")
            .replace(
                "Split {{input.topic}}",
                "Split {{input.subject}} for {{input.names}}",
            );
        let report = inspect(&bad, AGENTS, Lang::En);
        let all = report.warnings.join("\n");
        assert!(all.contains("claude-code"), "{all}");
        assert!(all.contains("list.json"), "{all}");
        assert!(all.contains("never stops"), "{all}");
        assert!(all.contains("input.subject"), "{all}");
        assert!(all.contains("private input names"), "{all}");
        let feedback = report.as_feedback(Lang::Zh).unwrap();
        assert!(feedback.starts_with("Atrium 校验"));
    }

    #[test]
    fn a_draft_that_does_not_read_says_so_in_chinese() {
        let report = inspect("id = \"x\"\nname = \"X\"\nsteps = []", AGENTS, Lang::Zh);
        assert!(report.template.is_none());
        assert_eq!(report.error.as_deref(), Some("模板里没有步骤"));
        let report = inspect("id = ", AGENTS, Lang::Zh);
        assert!(report.error.unwrap().starts_with("文件读不出来"));
    }

    #[test]
    fn the_brief_opens_with_the_request() {
        let text = brief("  每周读 arXiv 写简报 ", None, true, Lang::Zh);
        assert!(text.starts_with("每周读 arXiv 写简报\n"));
        assert!(
            text.contains(DRAFT_FILE) && text.contains(GUIDE_FILE) && text.contains(REFERENCES_DIR)
        );
        let base = Template::parse(DRAFT).unwrap();
        let text = brief("加一步配图", Some(&base), false, Lang::Zh);
        assert!(text.starts_with("修改工作流「Essay」：加一步配图"));
        assert!(!text.contains(REFERENCES_DIR));
    }

    #[test]
    fn item_placeholders_need_each() {
        let bad = DRAFT.replace("each = { from = \"parts.json\" }\n", "");
        let report = inspect(&bad, AGENTS, Lang::En);
        assert!(report.warnings.iter().any(|w| w.contains("no `each`")));
    }

    #[test]
    fn the_guide_names_every_field() {
        let fields = [
            "id",
            "name",
            "description",
            "version",
            "preamble",
            "agent",
            "effort",
            "attempts",
            "timeout_min",
            "key",
            "label",
            "help",
            "kind",
            "required",
            "options",
            "default",
            "private",
            "when",
            "title",
            "summary",
            "skill",
            "prompt",
            "needs",
            "outputs",
            "checks",
            "gate",
            "optional",
            "fixes",
            "decide",
            "each",
            "question",
            "from",
            "max",
        ];
        for field in fields {
            assert!(
                GUIDE.contains(&format!("{field} =")) || GUIDE.contains(&format!("{field}:")),
                "the guide leaves out {field}"
            );
        }
        for kind in [
            "exists",
            "contains",
            "absent",
            "command",
            "pdf_pages",
            "verdict",
            "review",
        ] {
            assert!(
                GUIDE.contains(&format!("kind = \"{kind}\"")),
                "the guide leaves out {kind}"
            );
        }
        for kind in ["files", "text", "long", "choice", "number"] {
            assert!(GUIDE.contains(kind));
        }
    }

    #[test]
    fn a_minimal_template_with_a_stop_is_clean() {
        let report = inspect(
            "id = \"x\"\nname = \"X\"\n[[steps]]\nid = \"a\"\ntitle = \"A\"\nprompt = \"p\"\noutputs = [\"a.md\"]\ngate = true",
            AGENTS,
            Lang::En,
        );
        assert!(report.is_clean());
    }
}

#[cfg(test)]
mod builtin_tests {
    use super::*;

    #[test]
    fn builtin_templates_lint_clean() {
        for (id, text) in crate::builtin::SOURCES {
            let report = inspect(
                text,
                &["codex", "claude", "deepseek", "antigravity"],
                Lang::En,
            );
            assert!(
                report.is_clean(),
                "{id}: {:?} {:?}",
                report.error,
                report.warnings
            );
        }
    }
}
