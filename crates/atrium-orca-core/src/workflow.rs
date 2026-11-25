//! `workflow.*`: templates, runs and their stops as JSON ops.
//!
//! The core is the run's state machine and nothing else. `workflow.run.next`
//! returns what the worker must do (start an agent, run a command check,
//! show a gate); the worker reports back with `report`, `check_result` and
//! `decide`. Every call reads `run.json` and writes it back, so the worker
//! can restart at any point. The run folder is the app's: the same files.

use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use atrium_workflow::{
    Check, Choice, InputKind, Template, TemplateError, author, builtin, check, read_choices,
    render::{self, Lang},
    run::{Attempt, CheckResult, Outcome, Run, RunState, StepRun, StepStatus},
    runner,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{now_ms, store::write_atomic};

// ---------- files ----------

fn run_file(dir: &Path) -> PathBuf {
    dir.join(".atrium").join("run.json")
}

fn load_run(dir: &Path) -> Result<Run, String> {
    let text = fs::read_to_string(run_file(dir))
        .map_err(|_| format!("{} has no workflow run", dir.display()))?;
    Run::from_json(&text).map_err(|e| format!("{}: run.json does not read: {e}", dir.display()))
}

fn save_run(dir: &Path, run: &Run) -> Result<(), String> {
    write_atomic(&run_file(dir), run.to_json().as_bytes()).map_err(|e| format!("{e:#}"))
}

/// One line of `.atrium/log.jsonl`, like the app's.
fn append_log(dir: &Path, event: Value) {
    use std::io::Write as _;
    let path = dir.join(".atrium").join("log.jsonl");
    let _ = fs::create_dir_all(dir.join(".atrium"));
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{event}");
    }
}

fn read_log(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join(".atrium").join("log.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect()
}

/// Check results that came back, by step then check index, until all are in.
fn pending_file(dir: &Path) -> PathBuf {
    dir.join(".atrium").join("orca-checks.json")
}

type Pending = BTreeMap<String, BTreeMap<usize, CheckResult>>;

fn read_pending(dir: &Path) -> Pending {
    fs::read_to_string(pending_file(dir))
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn write_pending(dir: &Path, pending: &Pending) -> Result<(), String> {
    if pending.is_empty() {
        let _ = fs::remove_file(pending_file(dir));
        return Ok(());
    }
    let text = serde_json::to_string(pending).map_err(|e| e.to_string())?;
    write_atomic(&pending_file(dir), text.as_bytes()).map_err(|e| format!("{e:#}"))
}

fn forget_pending(dir: &Path, step: &str) {
    let mut pending = read_pending(dir);
    if pending.remove(step).is_some() {
        let _ = write_pending(dir, &pending);
    }
}

/// The run step's id: `step`, or `step:item` for one item of a repeating step.
fn sid(step: &str, item: Option<&str>) -> String {
    match item {
        Some(item) if !step.contains(':') => format!("{step}:{item}"),
        _ => step.to_string(),
    }
}

fn lang_of(arg: Option<&str>, template: &Template) -> Lang {
    match arg {
        Some("zh") => Lang::Zh,
        Some("en") => Lang::En,
        _ => {
            let cjk = |s: &str| s.chars().any(|c| matches!(c as u32, 0x3400..=0x9fff));
            if cjk(&template.preamble) || cjk(&template.name) {
                Lang::Zh
            } else {
                Lang::En
            }
        }
    }
}

// ---------- templates ----------

/// A template folder: a path, or a path with where its templates come from.
#[derive(Deserialize)]
#[serde(untagged)]
pub enum DirArg {
    Path(PathBuf),
    Tagged {
        path: PathBuf,
        #[serde(default)]
        source: Option<String>,
    },
}

impl DirArg {
    fn path(&self) -> &Path {
        match self {
            DirArg::Path(p) | DirArg::Tagged { path: p, .. } => p,
        }
    }
}

struct Listed {
    id: String,
    template: Option<Template>,
    source: &'static str,
    path: Option<PathBuf>,
    error: Option<String>,
}

fn source_name(arg: &DirArg, index: usize) -> &'static str {
    match arg {
        DirArg::Tagged {
            source: Some(s), ..
        } if s == "project" => "project",
        DirArg::Tagged {
            source: Some(s), ..
        } if s == "user" => "user",
        _ if index == 0 => "user",
        _ => "project",
    }
}

/// Built-in templates, then each folder's `*.toml`; a later template with
/// the same id takes the earlier one's place.
fn scan(dirs: &[DirArg]) -> Vec<Listed> {
    let mut out: Vec<Listed> = builtin::all()
        .into_iter()
        .map(|t| Listed {
            id: t.id.clone(),
            template: Some(t),
            source: "builtin",
            path: None,
            error: None,
        })
        .collect();
    for (index, dir) in dirs.iter().enumerate() {
        let source = source_name(dir, index);
        let Ok(entries) = fs::read_dir(dir.path()) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for path in files {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = match fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    out.push(Listed {
                        id: stem,
                        template: None,
                        source,
                        path: Some(path),
                        error: Some(e.to_string()),
                    });
                    continue;
                }
            };
            match Template::parse(&text) {
                Ok(t) => {
                    out.retain(|e| e.id != t.id);
                    out.push(Listed {
                        id: t.id.clone(),
                        template: Some(t),
                        source,
                        path: Some(path),
                        error: None,
                    });
                }
                Err(error) => out.push(Listed {
                    id: stem,
                    template: None,
                    source,
                    path: Some(path),
                    error: Some(author::explain(&error, Lang::En)),
                }),
            }
        }
    }
    out
}

fn input_kind(kind: InputKind) -> &'static str {
    match kind {
        InputKind::Files => "files",
        InputKind::Text => "text",
        InputKind::Long => "long",
        InputKind::Choice => "choice",
        InputKind::Number => "number",
    }
}

fn has_commands(t: &Template) -> bool {
    t.steps.iter().any(|s| {
        s.checks
            .iter()
            .any(|c| matches!(c.check, Check::Command { .. }))
    })
}

fn summary(item: &Listed) -> Value {
    let mut out = json!({
        "id": item.id,
        "name": item.template.as_ref().map_or(item.id.as_str(), |t| t.name.as_str()),
        "source": item.source,
        "steps": item.template.as_ref().map_or(0, |t| t.steps.len()),
        "inputs": [],
    });
    let map = out.as_object_mut().expect("an object");
    if let Some(path) = &item.path {
        map.insert("path".into(), json!(path));
    }
    if let Some(error) = &item.error {
        map.insert("error".into(), json!(error));
    }
    if let Some(t) = &item.template {
        if !t.description.is_empty() {
            map.insert("description".into(), json!(t.description));
        }
        map.insert("version".into(), json!(t.version));
        map.insert("hasCommands".into(), json!(has_commands(t)));
        let inputs: Vec<Value> = t
            .inputs
            .iter()
            .map(|i| {
                let mut v = json!({
                    "id": i.key,
                    "label": i.label,
                    "kind": input_kind(i.kind),
                    "required": i.required,
                });
                let m = v.as_object_mut().expect("an object");
                let default = i.default.clone().or_else(|| i.options.first().cloned());
                if let Some(d) = default {
                    m.insert("default".into(), json!(d));
                }
                if !i.options.is_empty() {
                    m.insert("options".into(), json!(i.options));
                }
                if !i.help.is_empty() {
                    m.insert("help".into(), json!(i.help));
                }
                if let Some(w) = &i.when {
                    m.insert("when".into(), json!(w));
                }
                if i.private {
                    m.insert("private".into(), json!(true));
                }
                v
            })
            .collect();
        map.insert("inputs".into(), json!(inputs));
    }
    out
}

#[derive(Deserialize)]
pub struct TemplatesArgs {
    #[serde(default)]
    dirs: Vec<DirArg>,
}

/// `workflow.templates {dirs}` → `TemplateSummary[]`. The first folder is the
/// user's, the others the projects' (or tag them `{path, source}`).
pub fn templates(args: TemplatesArgs) -> Result<Value, String> {
    Ok(Value::Array(scan(&args.dirs).iter().map(summary).collect()))
}

#[derive(Deserialize)]
pub struct TextArgs {
    text: String,
}

/// `workflow.template.parse {text}` → `{template}` or `{template: null, errors}`.
pub fn template_parse(args: TextArgs) -> Result<Value, String> {
    let (template, errors) = check_text(&args.text);
    Ok(match template {
        Some(t) => json!({ "template": t, "errors": [] }),
        None => json!({ "template": null, "errors": errors }),
    })
}

#[derive(Deserialize)]
pub struct RenderArgs {
    template: Value,
}

/// `workflow.template.render {template}` → `{text}` (the TOML).
pub fn template_render(args: RenderArgs) -> Result<Value, String> {
    let template: Template =
        serde_json::from_value(args.template).map_err(|e| format!("bad template: {e}"))?;
    template
        .validate()
        .map_err(|e| author::explain(&e, Lang::En))?;
    Ok(json!({ "text": template.to_toml() }))
}

/// The 1-based line of the first line holding any of `needles`.
fn find_line(text: &str, needles: &[String]) -> Option<usize> {
    text.lines()
        .position(|l| needles.iter().any(|n| l.contains(n.as_str())))
        .map(|i| i + 1)
}

/// The template in `text`, or what is wrong with it, with line numbers.
fn check_text(text: &str) -> (Option<Template>, Vec<Value>) {
    let template: Template = match toml::from_str(text) {
        Ok(t) => t,
        Err(error) => {
            let line = error
                .span()
                .map(|s| text[..s.start.min(text.len())].matches('\n').count() + 1);
            let mut e = json!({ "message": error.message() });
            if let Some(line) = line {
                e["line"] = json!(line);
            }
            return (None, vec![e]);
        }
    };
    match template.validate() {
        Ok(()) => (Some(template), Vec::new()),
        Err(error) => {
            let ids: Vec<String> = match &error {
                TemplateError::DuplicateStep(id)
                | TemplateError::EmptyPrompt(id)
                | TemplateError::BadId(id)
                | TemplateError::DuplicateInput(id)
                | TemplateError::NoOptions(id) => vec![format!("\"{id}\"")],
                TemplateError::UnknownNeed { step, .. }
                | TemplateError::UnknownFix { step, .. }
                | TemplateError::BadCheck { step, .. } => vec![format!("id = \"{step}\"")],
                TemplateError::UnknownInput { what, .. } => {
                    vec![format!("id = \"{what}\""), format!("key = \"{what}\"")]
                }
                _ => Vec::new(),
            };
            let mut e = json!({ "message": author::explain(&error, Lang::En) });
            if let Some(line) = find_line(text, &ids) {
                e["line"] = json!(line);
            }
            (None, vec![e])
        }
    }
}

#[derive(Deserialize)]
pub struct ValidateArgs {
    text: String,
    /// Agent ids the app knows; with them, warnings about others are added.
    #[serde(default)]
    agents: Option<Vec<String>>,
    #[serde(default)]
    lang: Option<String>,
}

/// `workflow.template.validate {text, agents?}` → `{errors, warnings, steps?}`.
pub fn template_validate(args: ValidateArgs) -> Result<Value, String> {
    let (template, errors) = check_text(&args.text);
    let Some(template) = template else {
        return Ok(json!({ "errors": errors, "warnings": [] }));
    };
    let lang = lang_of(args.lang.as_deref(), &template);
    let warnings: Vec<String> = args.agents.as_ref().map_or_else(Vec::new, |agents| {
        let known: Vec<&str> = agents.iter().map(String::as_str).collect();
        author::lint(&template, &known, lang)
    });
    let run = preview_run(&template);
    Ok(json!({
        "errors": [],
        "warnings": warnings.iter().map(|w| json!({"message": w})).collect::<Vec<_>>(),
        "steps": step_views(None, &run, lang),
    }))
}

/// A run that has not started, to show a template's steps.
fn preview_run(template: &Template) -> Run {
    Run {
        template: template.clone(),
        inputs: HashMap::new(),
        state: RunState::Running,
        steps: template
            .steps
            .iter()
            .map(|s| StepRun {
                id: s.id.clone(),
                item: None,
                status: StepStatus::Pending,
                attempts: Vec::new(),
                feedback: Vec::new(),
                budget_from: 0,
                expanded: false,
            })
            .collect(),
        decisions: Vec::new(),
        experience: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

// ---------- starting a run ----------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartArgs {
    /// The template's id.
    template: String,
    /// Its file, when it is not built in.
    #[serde(default)]
    path: Option<PathBuf>,
    /// Or its text.
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    dirs: Vec<DirArg>,
    #[serde(default)]
    inputs: HashMap<String, String>,
    /// The new run folder.
    dir: PathBuf,
    /// Optional steps to leave out.
    #[serde(default)]
    skip: Vec<String>,
    #[serde(default)]
    now: Option<i64>,
}

fn resolve(args: &StartArgs) -> Result<Template, String> {
    if let Some(text) = &args.text {
        return Template::parse(text).map_err(|e| author::explain(&e, Lang::En));
    }
    if let Some(path) = &args.path {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        return Template::parse(&text).map_err(|e| author::explain(&e, Lang::En));
    }
    let found = scan(&args.dirs).into_iter().find(|l| l.id == args.template);
    match found {
        Some(Listed {
            template: Some(t), ..
        }) => Ok(t),
        Some(Listed { error: Some(e), .. }) => Err(e),
        _ => Err(format!("no template {}", args.template)),
    }
}

/// Copies `files` into `<dir>/inputs/`, renaming on clashes; their paths
/// relative to `dir`, one per line.
fn copy_inputs(dir: &Path, files: &[PathBuf]) -> Result<String, String> {
    let inputs = dir.join("inputs");
    fs::create_dir_all(&inputs).map_err(|e| e.to_string())?;
    let mut lines = Vec::new();
    for file in files {
        let name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();
        let mut target = inputs.join(&name);
        let mut n = 2;
        while target.exists() {
            let (stem, ext) = match name.rsplit_once('.') {
                Some((s, e)) => (s.to_string(), format!(".{e}")),
                None => (name.clone(), String::new()),
            };
            target = inputs.join(format!("{stem}-{n}{ext}"));
            n += 1;
        }
        if file.is_dir() {
            copy_dir(file, &target).map_err(|e| format!("{}: {e}", file.display()))?;
        } else {
            fs::copy(file, &target).map_err(|e| format!("{}: {e}", file.display()))?;
        }
        let rel = target
            .strip_prefix(dir)
            .unwrap_or(&target)
            .to_string_lossy()
            .replace('\\', "/");
        lines.push(rel);
    }
    Ok(lines.join("\n"))
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Lessons kept from earlier runs: `<first folder>/<id>.experience.md`.
fn experience_text(dirs: &[DirArg], id: &str) -> Option<String> {
    let dir = dirs.first()?.path();
    fs::read_to_string(dir.join(format!("{id}.experience.md")))
        .ok()
        .filter(|t| !t.trim().is_empty())
}

/// `workflow.run.start {template, path?, text?, dirs, inputs, dir, skip?}` →
/// `{dir, state}`. Files inputs given as absolute paths are copied in.
pub fn run_start(args: StartArgs) -> Result<Value, String> {
    let template = resolve(&args)?;
    let dir = args.dir.clone();
    if run_file(&dir).exists() {
        return Err(format!("{} already holds a run", dir.display()));
    }
    let mut values = args.inputs.clone();
    // A dry run first, so a missing answer says so before any folder exists.
    let mut dry = values.clone();
    for input in template
        .inputs
        .iter()
        .filter(|i| i.kind == InputKind::Files)
    {
        if values.get(&input.key).is_some_and(|v| !v.trim().is_empty()) {
            dry.insert(input.key.clone(), "-".into());
        }
    }
    Run::new(template.clone(), dry, &args.skip, 0).map_err(|e| match e {
        atrium_workflow::run::RunError::MissingInput(label) => format!("Fill in {label} first."),
        other => other.to_string(),
    })?;
    fs::create_dir_all(dir.join(".atrium")).map_err(|e| format!("{}: {e}", dir.display()))?;
    for input in template
        .inputs
        .iter()
        .filter(|i| i.kind == InputKind::Files)
    {
        let Some(raw) = values.get(&input.key).cloned() else {
            continue;
        };
        let (mut copy, mut keep) = (Vec::new(), Vec::new());
        for line in raw.lines().map(str::trim).filter(|l| !l.is_empty()) {
            let path = PathBuf::from(line);
            if path.is_absolute() && !path.starts_with(&dir) {
                copy.push(path);
            } else {
                keep.push(line.to_string());
            }
        }
        let copied = copy_inputs(&dir, &copy)?;
        keep.extend(copied.lines().map(str::to_string).filter(|l| !l.is_empty()));
        values.insert(input.key.clone(), keep.join("\n"));
    }
    write_atomic(
        &dir.join(".atrium").join("workflow.toml"),
        template.to_toml().as_bytes(),
    )
    .map_err(|e| format!("{e:#}"))?;
    let mut run = Run::new(template.clone(), values, &args.skip, now_ms(args.now))
        .map_err(|e| e.to_string())?;
    run.experience = experience_text(&args.dirs, &template.id);
    save_run(&dir, &run)?;
    append_log(
        &dir,
        json!({"at": run.created_ms, "event": "run_started", "template": template.id, "version": template.version}),
    );
    Ok(json!({ "dir": dir, "templateId": template.id, "state": state_name(&run) }))
}

#[derive(Deserialize)]
pub struct ExperienceArgs {
    /// The template's id.
    template: String,
    #[serde(default)]
    dirs: Vec<DirArg>,
}

/// `workflow.experience {template, dirs}` → `{notes: string[], text}`.
pub fn experience(args: ExperienceArgs) -> Result<Value, String> {
    let text = experience_text(&args.dirs, &args.template).unwrap_or_default();
    Ok(json!({ "notes": notes_of(&text), "text": text }))
}

/// The bullet lines of an experience file, or its non-empty lines.
fn notes_of(text: &str) -> Vec<String> {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let bullets: Vec<String> = lines
        .iter()
        .filter_map(|l| l.strip_prefix("- ").or_else(|| l.strip_prefix("* ")))
        .map(|l| l.trim().to_string())
        .collect();
    if bullets.is_empty() {
        lines
            .iter()
            .filter(|l| !l.starts_with('#'))
            .map(|l| l.to_string())
            .collect()
    } else {
        bullets
    }
}

// ---------- the run as the UI sees it ----------

fn state_name(run: &Run) -> &'static str {
    match run.state {
        RunState::Running => "running",
        RunState::Paused => "paused",
        RunState::Waiting
            if run.steps.iter().any(|s| s.status == StepStatus::Failed)
                && !run.steps.iter().any(|s| s.status == StepStatus::Review) =>
        {
            "failed"
        }
        RunState::Waiting => "waiting",
        RunState::Done => "done",
        RunState::Stopped => "cancelled",
    }
}

fn status_name(step: &StepRun) -> &'static str {
    match step.status {
        StepStatus::Pending
            if !step.attempts.is_empty()
                && step.budget_from == step.attempts.len()
                && !step.feedback.is_empty() =>
        {
            "sent_back"
        }
        StepStatus::Pending => "pending",
        StepStatus::Running => "running",
        StepStatus::Checking => "checking",
        StepStatus::Review => "waiting",
        StepStatus::Done => "passed",
        StepStatus::Failed => "failed",
        StepStatus::Skipped => "skipped",
    }
}

fn attempt_view(n: usize, a: &Attempt, last: bool, sent_back: bool) -> Value {
    let checks: Vec<Value> = a
        .checks
        .iter()
        .map(|c| {
            let mut v = json!({ "label": c.what, "ok": c.passed });
            if !c.detail.is_empty() {
                v["detail"] = json!(c.detail);
                v["log"] = json!(c.detail);
            }
            v
        })
        .collect();
    let mut v = json!({
        "n": n,
        "startedAt": a.started_ms,
        "checks": checks,
    });
    if let Some(end) = a.finished_ms {
        v["endedAt"] = json!(end);
        v["outcome"] = json!(if a.passed() {
            "passed"
        } else if last && sent_back {
            "sent_back"
        } else {
            "failed"
        });
    }
    if let Some(agent) = &a.agent {
        v["agent"] = json!(agent);
    }
    if let Some(report) = &a.answer {
        v["report"] = json!(report);
    }
    v
}

/// `StepView`s of the run's template steps (items nested in their step).
/// `lang` renders the prompts.
fn step_views(dir: Option<&Path>, run: &Run, lang: Lang) -> Vec<Value> {
    let mut out = Vec::new();
    for (ix, spec) in run.template.steps.iter().enumerate() {
        let Some(step) = run.step(&spec.id) else {
            continue;
        };
        let kind = if spec.each.is_some() {
            "each"
        } else if spec.gate || spec.decide.is_some() {
            "gate"
        } else {
            "agent"
        };
        let mut v = json!({
            "id": spec.id,
            "title": spec.title,
            "needs": run.template.needs_of(ix),
            "kind": kind,
            "status": status_name(step),
            "attempts": step.attempts.iter().enumerate().map(|(i, a)| {
                // A reviewer that failed and went back to the step it fixes.
                let sent_back = step.status == StepStatus::Pending && !spec.fixes.is_empty();
                attempt_view(i + 1, a, i + 1 == step.attempts.len(), sent_back)
            }).collect::<Vec<_>>(),
        });
        if spec.each.is_some() {
            v["items"] = json!(
                run.items_of(&spec.id)
                    .iter()
                    .map(|i| {
                        let item = i.item.as_ref();
                        json!({
                            "id": item.map_or(i.id.as_str(), |c| c.id.as_str()),
                            "label": item.map_or(i.id.as_str(), |c| c.title.as_str()),
                            "status": status_name(i),
                        })
                    })
                    .collect::<Vec<_>>()
            );
        } else if let Some(p) = render::prompt(run, &spec.id, true, lang) {
            v["prompt"] = json!(p);
        }
        if !spec.fixes.is_empty() {
            v["sendBack"] = json!(spec.fixes);
        }
        if step.status == StepStatus::Review
            && let Some(dir) = dir
        {
            let (choices, _) = review_choices(dir, run, &spec.id, lang);
            v["choices"] = json!(choices);
        }
        if !spec.summary.is_empty() {
            v["summary"] = json!(spec.summary);
        }
        out.push(v);
    }
    out
}

fn current_step(run: &Run) -> Option<String> {
    let live = |s: &&StepRun| {
        matches!(
            s.status,
            StepStatus::Running | StepStatus::Checking | StepStatus::Review | StepStatus::Failed
        ) && !s.expanded
    };
    run.steps
        .iter()
        .find(live)
        .or_else(|| {
            run.steps
                .iter()
                .find(|s| s.status == StepStatus::Pending && !s.is_item())
        })
        .map(|s| s.spec().to_string())
}

fn decisions_view(dir: &Path, run: &Run) -> Vec<Value> {
    let mut out: Vec<Value> = read_log(dir)
        .into_iter()
        .filter(|e| e["event"] == "decided")
        .map(|e| {
            let mut v = json!({
                "at": e["at"], "step": e["step"], "choice": e["choice"],
            });
            for key in ["sendBackTo", "note"] {
                if e.get(key).is_some_and(|x| !x.is_null()) {
                    v[key] = e[key].clone();
                }
            }
            v
        })
        .collect();
    for d in &run.decisions {
        if out.iter().any(|v| v["step"] == d.step.as_str()) {
            continue;
        }
        let mut v = json!({
            "at": d.at_ms,
            "step": d.step,
            "choice": d.choice.as_ref().map_or("approve", |c| c.id.as_str()),
        });
        if !d.note.is_empty() {
            v["note"] = json!(d.note);
        }
        out.push(v);
    }
    out.sort_by_key(|v| v["at"].as_i64().unwrap_or(0));
    out
}

#[derive(Deserialize)]
pub struct DirArgs {
    dir: PathBuf,
    #[serde(default)]
    lang: Option<String>,
}

/// `workflow.run.load {dir}` → `RunView`.
pub fn run_load(args: DirArgs) -> Result<Value, String> {
    let run = load_run(&args.dir)?;
    let lang = lang_of(args.lang.as_deref(), &run.template);
    let (done, total) = run.progress();
    let mut view = json!({
        "dir": args.dir,
        "templateId": run.template.id,
        "templateName": run.template.name,
        "state": state_name(&run),
        "startedAt": run.created_ms,
        "updatedAt": run.updated_ms,
        "progress": { "done": done, "total": total },
        "inputs": run.inputs,
        "steps": step_views(Some(&args.dir), &run, lang),
        "decisions": decisions_view(&args.dir, &run),
        "experience": notes_of(run.experience.as_deref().unwrap_or("")),
    });
    if let Some(step) = current_step(&run) {
        view["currentStep"] = json!(step);
    }
    Ok(view)
}

// ---------- driving a run ----------

/// Checks of step `id` the core cannot run, with their index in `checks_of`.
fn outside_checks(run: &Run, id: &str) -> Vec<(usize, Check)> {
    run.checks_of(id)
        .into_iter()
        .enumerate()
        .filter(|(_, c)| !c.is_local())
        .collect()
}

/// Ends the checks of `id` with `outside` results added after the local ones.
fn finish_checks(
    dir: &Path,
    run: &mut Run,
    id: &str,
    outside: &BTreeMap<usize, CheckResult>,
    lang: Lang,
    now: i64,
) -> Result<Outcome, String> {
    let mut results = check::local_results(dir, run, id, lang);
    let checks = run.checks_of(id);
    for (index, result) in outside {
        if let Some(c) = checks.get(*index) {
            results.push(CheckResult {
                what: check::describe(c, lang),
                ..result.clone()
            });
        }
    }
    let outcome = run
        .checked(id, results.clone(), now)
        .map_err(|e| e.to_string())?;
    append_log(
        dir,
        json!({"at": now, "event": "checked", "step": id, "outcome": format!("{outcome:?}"), "results": results}),
    );
    forget_pending(dir, id);
    Ok(outcome)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextArgs {
    dir: PathBuf,
    #[serde(default)]
    lang: Option<String>,
    /// Agents that can use installed skills; default: the Claude ones.
    #[serde(default)]
    skills: Option<bool>,
    /// Steps running at once, counting those already started.
    #[serde(default)]
    max_parallel: Option<usize>,
    #[serde(default)]
    now: Option<i64>,
}

fn short(text: &str, max: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max).collect();
    cut.push('…');
    cut
}

fn review_choices(dir: &Path, run: &Run, id: &str, lang: Lang) -> (Vec<Value>, Vec<Choice>) {
    let zh = lang == Lang::Zh;
    let spec = run.spec(id).expect("a spec of the step");
    if spec.decide.is_some() {
        let options = run
            .options_file(id)
            .and_then(|file| {
                atrium_workflow::files::read_text(
                    dir,
                    file,
                    atrium_workflow::files::DEFAULT_BYTE_CAP,
                )
                .ok()
            })
            .map(|text| read_choices(&text))
            .unwrap_or_default();
        let choices = options
            .iter()
            .map(|o| json!({"id": o.id, "label": o.title}))
            .collect();
        (choices, options)
    } else {
        (
            vec![json!({"id": "approve", "label": if zh {"通过"} else {"Approve"}})],
            Vec::new(),
        )
    }
}

fn gate_for(run: &Run, id: &str, dir: &Path, lang: Lang) -> Value {
    let zh = lang == Lang::Zh;
    let step = run.step(id).expect("a step of the run");
    let spec = run.spec(id).expect("a spec of the step");
    let title = run.title_of(id);
    let failed = step.status == StepStatus::Failed;
    let mut detail = String::new();
    let mut choices: Vec<Value> = Vec::new();
    if failed {
        if let Some(last) = step.last() {
            let faults: Vec<String> = last
                .checks
                .iter()
                .filter(|c| !c.passed)
                .map(|c| {
                    if c.detail.is_empty() {
                        format!("- {}", c.what)
                    } else {
                        format!("- {}: {}", c.what, short(&c.detail, 300))
                    }
                })
                .collect();
            detail = faults.join("\n");
        }
        choices.push(json!({"id": "retry", "label": if zh {"再试一次"} else {"Try again"}}));
        if spec.optional || step.is_item() {
            choices.push(json!({"id": "skip", "label": if zh {"跳过"} else {"Skip"}}));
        }
    } else {
        if let Some(answer) = run.answer_of(id) {
            detail = short(answer, 1200);
        }
        let (c, options) = review_choices(dir, run, id, lang);
        choices = c;
        if spec.decide.is_some() {
            let list: Vec<String> = options
                .iter()
                .map(|o| {
                    if o.summary.is_empty() {
                        format!("- {}", o.title)
                    } else {
                        format!("- {}: {}", o.title, o.summary)
                    }
                })
                .collect();
            let question = spec.decide.as_ref().map_or("", |d| d.question.as_str());
            detail = format!("{question}\n{}\n\n{detail}", list.join("\n"))
                .trim()
                .to_string();
        }
    }
    // Where it may send work back to: its own fixes, then what is done before it.
    let at = run
        .template
        .steps
        .iter()
        .position(|s| s.id == spec.id)
        .unwrap_or(0);
    let mut back: Vec<String> = spec.fixes.clone();
    for earlier in &run.template.steps[..at] {
        let done = run
            .step(&earlier.id)
            .is_some_and(|s| s.status == StepStatus::Done);
        if done && !back.contains(&earlier.id) {
            back.push(earlier.id.clone());
        }
    }
    let send_back: Vec<Value> = back
        .iter()
        .map(|b| json!({"step": b, "label": run.title_of(b)}))
        .collect();
    json!({
        "kind": "gate",
        "step": id,
        "title": format!("{} · {title}", run.template.name),
        "detail": detail,
        "reason": if failed { "failed" } else { "review" },
        "choices": choices,
        "sendBack": send_back,
    })
}

/// `workflow.run.next {dir, lang?, skills?, maxParallel?}` → `{state, effects}`.
///
/// Lays out repeating steps, finishes checks that only read files, starts
/// ready steps (their try begins here) and returns what is outstanding:
/// `launch {step, item?, prompt, agent?}` for every running step with no
/// report yet, `check {step, item?, index, check}` for the checks the worker
/// must run, `gate {step, title, detail, choices, sendBack}` for stops, and
/// `done`. Calling it again returns the same outstanding work.
pub fn run_next(args: NextArgs) -> Result<Value, String> {
    let dir = &args.dir;
    let mut run = load_run(dir)?;
    let lang = lang_of(args.lang.as_deref(), &run.template);
    let now = now_ms(args.now);
    let mut changed = false;
    // Expand repeating steps and finish what needs no outside help.
    for _ in 0..64 {
        let mut again = false;
        for (id, from) in run.to_expand() {
            let text = atrium_workflow::files::read_text(
                dir,
                &from,
                atrium_workflow::files::DEFAULT_BYTE_CAP,
            )
            .unwrap_or_default();
            let items = read_choices(&text);
            let count = items.len();
            let result = run.expand(&id, items, now);
            append_log(
                dir,
                json!({"at": now, "event": "expanded", "step": id, "items": count}),
            );
            changed = true;
            again = again || result.is_ok();
        }
        let waiting: Vec<String> = run
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Checking)
            .map(|s| s.id.clone())
            .collect();
        for id in waiting {
            if outside_checks(&run, &id).is_empty() {
                finish_checks(dir, &mut run, &id, &BTreeMap::new(), lang, now)?;
                changed = true;
                again = true;
            }
        }
        if !again {
            break;
        }
    }
    // Begin ready steps, up to the parallel limit.
    let limit = args.max_parallel.unwrap_or(4).max(1);
    let mut running = run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Running && !s.expanded)
        .count();
    for id in run.ready() {
        if running >= limit {
            break;
        }
        let agent = run.spec(&id).and_then(|s| run.template.agent_of(s));
        if run.begin(&id, agent, now).is_ok() {
            append_log(dir, json!({"at": now, "event": "step_started", "step": id}));
            running += 1;
            changed = true;
        }
    }
    if changed {
        save_run(dir, &run)?;
    }

    let mut effects: Vec<Value> = Vec::new();
    let pending = read_pending(dir);
    for s in &run.steps {
        if s.expanded {
            continue;
        }
        let id = s.id.as_str();
        let (step, item) = match &s.item {
            Some(item) => (s.spec().to_string(), Some(item.id.clone())),
            None => (id.to_string(), None),
        };
        match s.status {
            StepStatus::Running => {
                let agent = s.last().and_then(|a| a.agent.clone());
                let skills = args
                    .skills
                    .unwrap_or_else(|| agent.as_deref().is_none_or(|a| a.starts_with("claude")));
                let Some(prompt) = render::prompt(&run, id, skills, lang) else {
                    continue;
                };
                let mut e = json!({"kind": "launch", "step": step, "prompt": prompt});
                if let Some(item) = &item {
                    e["item"] = json!(item);
                }
                if let Some(agent) = agent {
                    e["agent"] = json!(agent);
                }
                if let Some(effort) = run.spec(id).and_then(|sp| sp.effort.clone()) {
                    e["effort"] = json!(effort);
                }
                effects.push(e);
            }
            StepStatus::Checking => {
                let have = pending.get(id);
                for (index, c) in outside_checks(&run, id) {
                    if have.is_some_and(|h| h.contains_key(&index)) {
                        continue;
                    }
                    let mut spec = serde_json::to_value(&c).map_err(|e| e.to_string())?;
                    spec["what"] = json!(check::describe(&c, lang));
                    if let Check::Review {
                        rubric,
                        files,
                        agent,
                    } = &c
                    {
                        let fixes = run.spec(id).map(|sp| sp.fixes.clone()).unwrap_or_default();
                        spec["prompt"] = json!(runner::review_prompt(rubric, files, &fixes));
                        if agent.is_none()
                            && let Some(a) = run.spec(id).and_then(|sp| run.template.agent_of(sp))
                        {
                            spec["agent"] = json!(a);
                        }
                    }
                    let mut e =
                        json!({"kind": "check", "step": step, "index": index, "check": spec});
                    if let Some(item) = &item {
                        e["item"] = json!(item);
                    }
                    effects.push(e);
                }
            }
            StepStatus::Review | StepStatus::Failed => effects.push(gate_for(&run, id, dir, lang)),
            _ => {}
        }
    }
    match run.state {
        RunState::Done => effects.push(json!({"kind": "done", "state": "done"})),
        RunState::Stopped => effects.push(json!({"kind": "done", "state": "cancelled"})),
        _ if effects.is_empty() => effects.push(json!({"kind": "wait"})),
        _ => {}
    }
    Ok(json!({ "state": state_name(&run), "effects": effects }))
}

#[derive(Deserialize)]
pub struct StepArgs {
    dir: PathBuf,
    step: String,
    #[serde(default)]
    item: Option<String>,
}

#[derive(Deserialize)]
pub struct ReportArgs {
    #[serde(flatten)]
    at: StepArgs,
    answer: String,
    #[serde(default)]
    now: Option<i64>,
}

/// `workflow.run.report {dir, step, item?, answer}`: the agent's turn ended.
pub fn run_report(args: ReportArgs) -> Result<Value, String> {
    let dir = &args.at.dir;
    let id = sid(&args.at.step, args.at.item.as_deref());
    let mut run = load_run(dir)?;
    run.set_answer(&id, args.answer)
        .map_err(|e| e.to_string())?;
    run.turn_finished(&id, now_ms(args.now))
        .map_err(|e| e.to_string())?;
    save_run(dir, &run)?;
    append_log(
        dir,
        json!({"at": run.updated_ms, "event": "reported", "step": id}),
    );
    Ok(json!({ "state": state_name(&run) }))
}

#[derive(Deserialize)]
pub struct CheckResultArgs {
    #[serde(flatten)]
    at: StepArgs,
    #[serde(default)]
    index: Option<usize>,
    result: ResultArg,
    #[serde(default)]
    lang: Option<String>,
    #[serde(default)]
    now: Option<i64>,
}

#[derive(Deserialize)]
pub struct ResultArg {
    #[serde(default)]
    what: String,
    passed: bool,
    #[serde(default)]
    detail: String,
    #[serde(default, alias = "sendBack")]
    send_back: Option<String>,
}

/// `workflow.run.check_result {dir, step, item?, index?, result}`: one check
/// the worker ran. When the last one is in, the step's outcome is applied
/// and `{outcome}` says what happened (`done`, `review`, `retry`, `sent_back`,
/// `failed`); until then `{outcome: null}`.
pub fn run_check_result(args: CheckResultArgs) -> Result<Value, String> {
    let dir = &args.at.dir;
    let id = sid(&args.at.step, args.at.item.as_deref());
    let mut run = load_run(dir)?;
    let lang = lang_of(args.lang.as_deref(), &run.template);
    let now = now_ms(args.now);
    if run.step(&id).map(|s| s.status) != Some(StepStatus::Checking) {
        return Err(format!("step {id} is not being checked"));
    }
    let outside = outside_checks(&run, &id);
    let mut pending = read_pending(dir);
    let mine = pending.entry(id.clone()).or_default();
    let index = args
        .index
        .filter(|i| outside.iter().any(|(o, _)| o == i))
        .or_else(|| {
            outside
                .iter()
                .map(|(o, _)| *o)
                .find(|o| !mine.contains_key(o))
        })
        .ok_or_else(|| format!("step {id} has no check waiting for a result"))?;
    mine.insert(
        index,
        CheckResult {
            what: args.result.what,
            passed: args.result.passed,
            detail: args.result.detail,
            send_back: args.result.send_back,
        },
    );
    if outside.iter().all(|(o, _)| mine.contains_key(o)) {
        let all = mine.clone();
        let outcome = finish_checks(dir, &mut run, &id, &all, lang, now)?;
        save_run(dir, &run)?;
        return Ok(json!({ "outcome": outcome_name(&outcome), "state": state_name(&run) }));
    }
    write_pending(dir, &pending)?;
    Ok(json!({ "outcome": null, "state": state_name(&run) }))
}

fn outcome_name(outcome: &Outcome) -> &'static str {
    match outcome {
        Outcome::Done => "done",
        Outcome::Review => "review",
        Outcome::Retry => "retry",
        Outcome::SentBack { .. } => "sent_back",
        Outcome::Failed => "failed",
    }
}

#[derive(Deserialize)]
pub struct DecideArgs {
    #[serde(flatten)]
    at: StepArgs,
    /// `approve`, an option id at a deciding step, `retry` or `skip` at a
    /// failed step.
    #[serde(default)]
    choice: Option<String>,
    #[serde(default, alias = "sendBackTo")]
    send_back_to: Option<String>,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    now: Option<i64>,
}

/// `workflow.run.decide {dir, step, choice, send_back_to?, note?}`: the
/// user's word at a stop. With `send_back_to` the step goes back to that
/// earlier step with the note (and everything after it runs again).
pub fn run_decide(args: DecideArgs) -> Result<Value, String> {
    let dir = &args.at.dir;
    let id = sid(&args.at.step, args.at.item.as_deref());
    let mut run = load_run(dir)?;
    let now = now_ms(args.now);
    let note = args.note.clone().unwrap_or_default();
    let choice = args.choice.clone().unwrap_or_else(|| "approve".into());
    if let Some(target) = args.send_back_to.as_deref().filter(|t| !t.is_empty()) {
        run.redo(target, &note, now).map_err(|e| e.to_string())?;
        forget_pending(dir, target);
    } else {
        match choice.as_str() {
            "retry" => {
                run.redo(&id, &note, now).map_err(|e| e.to_string())?;
                forget_pending(dir, &id);
            }
            "skip" => run.skip(&id, now).map_err(|e| e.to_string())?,
            _ => {
                let picked = match run.options_file(&id) {
                    Some(file) => {
                        let text = atrium_workflow::files::read_text(
                            dir,
                            file,
                            atrium_workflow::files::DEFAULT_BYTE_CAP,
                        )
                        .unwrap_or_default();
                        read_choices(&text).into_iter().find(|c| c.id == choice)
                    }
                    None => None,
                };
                if run.options_file(&id).is_some() && picked.is_none() {
                    return Err(format!("{choice} is not one of the options of {id}"));
                }
                run.approve(&id, picked, &note, now).map_err(|e| match e {
                    atrium_workflow::run::RunError::ChoiceNeeded(_) => {
                        "Pick one of the options first.".to_string()
                    }
                    other => other.to_string(),
                })?;
            }
        }
    }
    save_run(dir, &run)?;
    let mut event = json!({"at": now, "event": "decided", "step": id, "choice": choice});
    if let Some(t) = &args.send_back_to {
        event["sendBackTo"] = json!(t);
    }
    if !note.is_empty() {
        event["note"] = json!(note);
    }
    append_log(dir, event);
    Ok(json!({ "state": state_name(&run) }))
}

#[derive(Deserialize)]
pub struct RetryArgs {
    #[serde(flatten)]
    at: StepArgs,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    now: Option<i64>,
}

/// `workflow.run.retry {dir, step, item?, note?}`: a running or checking
/// step starts its try again (not counted against it); a failed, stopped or
/// finished step goes again with the note.
pub fn run_retry(args: RetryArgs) -> Result<Value, String> {
    let dir = &args.at.dir;
    let id = sid(&args.at.step, args.at.item.as_deref());
    let mut run = load_run(dir)?;
    let now = now_ms(args.now);
    let status = run
        .step(&id)
        .map(|s| s.status)
        .ok_or_else(|| format!("no step {id}"))?;
    match status {
        StepStatus::Running | StepStatus::Checking => {
            run.recover_step(&id, now).map_err(|e| e.to_string())?
        }
        StepStatus::Failed | StepStatus::Review | StepStatus::Done => run
            .redo(&id, args.note.as_deref().unwrap_or(""), now)
            .map_err(|e| e.to_string())?,
        StepStatus::Pending | StepStatus::Skipped => {}
    }
    forget_pending(dir, &id);
    save_run(dir, &run)?;
    Ok(json!({ "state": state_name(&run) }))
}

/// `workflow.run.recover {dir}`: after a restart, steps cut off mid-turn or
/// mid-check start their try again.
pub fn run_recover(args: DirArgs) -> Result<Value, String> {
    let mut run = load_run(&args.dir)?;
    run.recover(now_ms(None));
    let _ = fs::remove_file(pending_file(&args.dir));
    save_run(&args.dir, &run)?;
    Ok(json!({ "state": state_name(&run) }))
}

pub enum Control {
    Pause,
    Resume,
    Cancel,
}

/// `workflow.run.pause|resume|cancel {dir}` → `{state}`.
pub fn run_control(args: DirArgs, control: Control) -> Result<Value, String> {
    let mut run = load_run(&args.dir)?;
    let now = now_ms(None);
    match control {
        Control::Pause => run.pause(now),
        Control::Resume => run.resume(now),
        Control::Cancel => run.stop(now),
    }
    save_run(&args.dir, &run)?;
    append_log(
        &args.dir,
        json!({"at": now, "event": "run_changed", "state": state_name(&run)}),
    );
    Ok(json!({ "state": state_name(&run) }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch;

    const TEMPLATE: &str = r#"
id = "essay"
name = "Essay"

[[inputs]]
key = "topic"
label = "Topic"
kind = "text"
required = true

[[steps]]
id = "outline"
title = "Outline"
prompt = "Outline an essay on {{input.topic}}."
outputs = ["outline.md"]
gate = true

[[steps]]
id = "draft"
title = "Draft"
prompt = "Write it."
outputs = ["essay.md"]
checks = [{ kind = "command", run = "make-pdf" }]

[[steps]]
id = "cases"
title = "Cases"
prompt = "Solve {{item.title}}."
each = { from = "items.json" }
outputs = ["out/{{item.id}}.txt"]

[[steps]]
id = "final"
title = "Final"
prompt = "Check it."
gate = true
fixes = "outline"
"#;

    fn call(op: &str, args: Value) -> Value {
        dispatch(op, args).unwrap_or_else(|e| panic!("{op}: {e}"))
    }

    fn effects(dir: &Path) -> Vec<Value> {
        call("workflow.run.next", json!({ "dir": dir, "now": 5 }))["effects"]
            .as_array()
            .unwrap()
            .clone()
    }

    fn one(dir: &Path, kind: &str) -> Value {
        let list = effects(dir);
        list.iter()
            .find(|e| e["kind"] == kind)
            .cloned()
            .unwrap_or_else(|| panic!("no {kind} in {list:?}"))
    }

    #[test]
    fn a_run_goes_from_launches_to_gates_checks_and_send_backs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        // A missing required input is refused before anything is made.
        assert!(
            dispatch(
                "workflow.run.start",
                json!({ "template": "essay", "text": TEMPLATE, "inputs": {}, "dir": dir })
            )
            .is_err()
        );
        assert!(!dir.exists());
        call(
            "workflow.run.start",
            json!({ "template": "essay", "text": TEMPLATE, "inputs": {"topic": "tea"}, "dir": dir }),
        );
        let launch = one(&dir, "launch");
        assert_eq!(launch["step"], "outline");
        assert!(launch["prompt"].as_str().unwrap().contains("essay on tea"));
        // Asking again returns the same work and starts nothing new.
        assert_eq!(effects(&dir).len(), 1);

        fs::write(dir.join("outline.md"), "1. Leaves").unwrap();
        call(
            "workflow.run.report",
            json!({ "dir": dir, "step": "outline", "answer": "Outlined." }),
        );
        let gate = one(&dir, "gate");
        assert_eq!(gate["step"], "outline");
        assert_eq!(gate["choices"][0]["id"], "approve");
        assert!(gate["detail"].as_str().unwrap().contains("Outlined."));
        let view = call("workflow.run.load", json!({ "dir": dir }));
        assert_eq!(view["state"], "waiting");
        assert_eq!(view["steps"][0]["status"], "waiting");
        assert_eq!(view["steps"][0]["kind"], "gate");
        assert_eq!(view["steps"][0]["attempts"][0]["report"], "Outlined.");

        call(
            "workflow.run.decide",
            json!({ "dir": dir, "step": "outline", "choice": "approve" }),
        );
        assert_eq!(one(&dir, "launch")["step"], "draft");
        fs::write(dir.join("essay.md"), "Tea.").unwrap();
        call(
            "workflow.run.report",
            json!({ "dir": dir, "step": "draft", "answer": "Drafted." }),
        );
        let check = one(&dir, "check");
        assert_eq!(check["check"]["kind"], "command");
        assert_eq!(check["check"]["run"], "make-pdf");
        assert_eq!(check["index"], 0);
        let out = call(
            "workflow.run.check_result",
            json!({ "dir": dir, "step": "draft", "index": 0, "result": {"what": "make", "passed": false, "detail": "exit 2"} }),
        );
        assert_eq!(out["outcome"], "retry");
        let again = one(&dir, "launch");
        assert_eq!(again["step"], "draft");
        assert!(
            again["prompt"].as_str().unwrap().contains("exit 2"),
            "the reasons go back"
        );
        call(
            "workflow.run.report",
            json!({ "dir": dir, "step": "draft", "answer": "Fixed." }),
        );
        let out = call(
            "workflow.run.check_result",
            json!({ "dir": dir, "step": "draft", "result": {"passed": true} }),
        );
        assert_eq!(out["outcome"], "done");

        // A repeating step lays out its items once their list exists.
        fs::write(
            dir.join("items.json"),
            r#"[{"id":"q1","title":"Q one"},{"id":"q2","title":"Q two"}]"#,
        )
        .unwrap();
        let launches: Vec<_> = effects(&dir)
            .into_iter()
            .filter(|e| e["kind"] == "launch")
            .collect();
        assert_eq!(launches.len(), 2);
        assert_eq!(
            (launches[0]["step"].as_str(), launches[0]["item"].as_str()),
            (Some("cases"), Some("q1"))
        );
        assert!(
            launches[1]["prompt"]
                .as_str()
                .unwrap()
                .contains("Solve Q two.")
        );
        let view = call("workflow.run.load", json!({ "dir": dir }));
        assert_eq!(view["steps"][2]["items"][1]["label"], "Q two");
        assert_eq!(view["steps"][2]["items"][0]["status"], "running");
        fs::create_dir_all(dir.join("out")).unwrap();
        for id in ["q1", "q2"] {
            fs::write(dir.join("out").join(format!("{id}.txt")), "ok").unwrap();
            call(
                "workflow.run.report",
                json!({ "dir": dir, "step": "cases", "item": id, "answer": "Solved." }),
            );
        }
        assert_eq!(one(&dir, "launch")["step"], "final");
        call(
            "workflow.run.report",
            json!({ "dir": dir, "step": "final", "answer": "Looks thin." }),
        );
        // The last stop: send the run back to the outline with a note.
        let gate = one(&dir, "gate");
        assert_eq!(gate["step"], "final");
        let targets: Vec<&str> = gate["sendBack"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["step"].as_str().unwrap())
            .collect();
        assert_eq!(targets[0], "outline", "its own fixes first");
        assert!(targets.contains(&"draft"));
        call(
            "workflow.run.decide",
            json!({ "dir": dir, "step": "final", "choice": "approve", "send_back_to": "outline", "note": "Add a history section." }),
        );
        let launch = one(&dir, "launch");
        assert_eq!(launch["step"], "outline");
        assert!(
            launch["prompt"]
                .as_str()
                .unwrap()
                .contains("Add a history section.")
        );
        let view = call("workflow.run.load", json!({ "dir": dir }));
        assert_eq!(view["steps"][0]["status"], "running");
        assert_eq!(
            view["steps"][1]["status"], "pending",
            "what depends on it runs again"
        );
        assert_eq!(view["decisions"][1]["sendBackTo"], "outline");
        assert_eq!(view["currentStep"], "outline");

        // Pause, resume and cancel.
        assert_eq!(
            call("workflow.run.pause", json!({ "dir": dir }))["state"],
            "paused"
        );
        assert_eq!(
            call("workflow.run.resume", json!({ "dir": dir }))["state"],
            "running"
        );
        assert_eq!(
            call("workflow.run.cancel", json!({ "dir": dir }))["state"],
            "cancelled"
        );
        assert_eq!(effects(&dir).last().unwrap()["kind"], "done");
        // The app reads the same files.
        assert!(Run::from_json(&fs::read_to_string(run_file(&dir)).unwrap()).is_ok());
        assert!(dir.join(".atrium").join("workflow.toml").is_file());
    }

    #[test]
    fn templates_list_with_builtins_and_errors_and_validate_with_lines() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("essay.toml"), TEMPLATE).unwrap();
        fs::write(tmp.path().join("broken.toml"), "id = \"b\"\nname = ").unwrap();
        let list = call("workflow.templates", json!({ "dirs": [tmp.path()] }));
        let list = list.as_array().unwrap();
        let find = |id: &str| {
            list.iter()
                .find(|t| t["id"] == id)
                .unwrap_or_else(|| panic!("{id} in {list:?}"))
        };
        assert_eq!(find("mathmodel")["source"], "builtin");
        assert!(find("mathmodel")["steps"].as_u64().unwrap() > 5);
        assert_eq!(find("essay")["source"], "user");
        assert_eq!(find("essay")["inputs"][0]["required"], true);
        assert!(find("broken")["error"].is_string());

        let bad = TEMPLATE.replace("fixes = \"outline\"", "fixes = \"later\"");
        let out = call("workflow.template.validate", json!({ "text": bad }));
        assert_eq!(out["errors"].as_array().unwrap().len(), 1);
        assert!(out["errors"][0]["line"].as_u64().is_some(), "{out}");
        let good = call("workflow.template.validate", json!({ "text": TEMPLATE }));
        assert_eq!(good["errors"], json!([]));
        assert_eq!(good["steps"].as_array().unwrap().len(), 4);
        let syntax = call("workflow.template.validate", json!({ "text": "id = " }));
        assert_eq!(syntax["errors"][0]["line"], 1);

        let parsed = call("workflow.template.parse", json!({ "text": TEMPLATE }));
        let text = call(
            "workflow.template.render",
            json!({ "template": parsed["template"] }),
        );
        assert_eq!(
            Template::parse(text["text"].as_str().unwrap()).unwrap(),
            Template::parse(TEMPLATE).unwrap()
        );
    }

    #[test]
    fn the_builtin_cumcm_template_starts_and_its_first_step_launches() {
        let tmp = tempfile::tempdir().unwrap();
        let problem = tmp.path().join("A.pdf");
        fs::write(&problem, "%PDF").unwrap();
        let dir = tmp.path().join("run");
        call(
            "workflow.run.start",
            json!({ "template": "mathmodel", "inputs": {"problem": problem, "contest": "CUMCM"}, "dir": dir }),
        );
        assert!(
            dir.join("inputs").join("A.pdf").is_file(),
            "files are copied in"
        );
        let launch = one(&dir, "launch");
        assert_eq!(
            launch["step"], "analysis",
            "the problem-picking step is skipped"
        );
        assert!(launch["prompt"].as_str().unwrap().contains("inputs/A.pdf"));
    }

    #[test]
    fn gate_step_load_returns_custom_choices() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("run");
        let tpl = r#"
id = "route"
name = "Route"

[[steps]]
id = "pick"
title = "Pick"
prompt = "Propose routes."

[steps.decide]
options = "routes.json"
question = "Which route?"

[[steps]]
id = "build"
title = "Build"
needs = ["pick"]
prompt = "Build {{step.pick.choice}}."
"#;
        call(
            "workflow.run.start",
            json!({ "template": "route", "text": tpl, "inputs": {}, "dir": dir }),
        );
        let _ = one(&dir, "launch");
        fs::write(
            dir.join("routes.json"),
            r#"[{"id":"fast","title":"Fast route","summary":"Quick"},{"id":"safe","title":"Safe route"}]"#,
        )
        .unwrap();
        call(
            "workflow.run.report",
            json!({ "dir": dir, "step": "pick", "answer": "Two routes." }),
        );
        let gate = one(&dir, "gate");
        assert_eq!(gate["step"], "pick");
        let view = call("workflow.run.load", json!({ "dir": dir }));
        assert_eq!(view["steps"][0]["status"], "waiting");
        assert_eq!(
            view["steps"][0]["choices"],
            json!([
                {"id": "fast", "label": "Fast route"},
                {"id": "safe", "label": "Safe route"}
            ])
        );
        let err = dispatch(
            "workflow.run.decide",
            json!({ "dir": dir, "step": "pick", "choice": "approve" }),
        );
        assert!(err.is_err(), "expected approve to be rejected: {err:?}");
        call(
            "workflow.run.decide",
            json!({ "dir": dir, "step": "pick", "choice": "safe" }),
        );
        assert_eq!(one(&dir, "launch")["step"], "build");
    }
}
