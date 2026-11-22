//! Workflows on the workspace: the templates to pick from, the runs and
//! what the user decides at their stops. This side keeps the files (the
//! run folder, its template snapshot and `run.json`); the runner that
//! starts agents on ready steps and checks their work is the backend's
//! (`docs/workflow-implementation.md`) and drives the same files.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use atrium_workflow::{
    InputKind, Template,
    author::{self, Report},
    builtin,
    render::Lang,
    run::Run,
};
use gpui::Context;

use super::{Notice, Room, Workspace};
use crate::i18n::{t, tf};

/// Where a template comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateSource {
    Builtin,
    User(PathBuf),
    Folder(PathBuf),
}

#[derive(Clone, Debug)]
pub struct TemplateEntry {
    pub template: Template,
    pub source: TemplateSource,
}

/// The form for writing a template with an agent: a new one, or a
/// change to `base`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Authoring {
    pub base: Option<String>,
}

/// The visual template editor: on the template with `id`, or on a new
/// one. `nonce` tells one opening from the next.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Editing {
    pub id: Option<String>,
    pub nonce: u64,
}

/// A run as the list shows it.
#[derive(Clone, Debug)]
pub struct RunEntry {
    pub dir: PathBuf,
    pub run: Run,
}

impl RunEntry {
    pub fn title(&self) -> String {
        self.dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&self.run.template.name)
            .to_string()
    }
}

pub(super) fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn user_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Atrium").join("workflows"))
}

fn lang() -> Lang {
    if crate::i18n::chinese() {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// Where templates being written with an agent live, one folder each.
pub fn drafts_dir() -> Option<PathBuf> {
    Some(user_dir()?.join("drafts"))
}

/// The draft folder a chat works in, when it is one.
pub fn draft_of(cwd: &str) -> Option<PathBuf> {
    let drafts = drafts_dir()?;
    let cwd = Path::new(cwd);
    (cwd.parent() == Some(drafts.as_path())).then(|| cwd.to_path_buf())
}

/// What Atrium makes of a draft folder's template; `None` before the
/// agent has written one.
pub fn draft_report(dir: &Path) -> Option<Report> {
    let text = std::fs::read_to_string(dir.join(author::DRAFT_FILE)).ok()?;
    if text.trim().is_empty() {
        return None;
    }
    Some(author::inspect(&text, AGENT_IDS, lang()))
}

/// The agents a template may name.
pub const AGENT_IDS: &[&str] = &["codex", "claude", "deepseek", "antigravity"];

/// A new draft folder under `parent`: the format guide, a full example,
/// the template being changed (when one is), and the user's references.
fn make_draft(
    parent: &Path,
    name: &str,
    base: Option<&str>,
    references: &[PathBuf],
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(parent)?;
    let dir = free_dir(parent, name);
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(author::GUIDE_FILE), author::GUIDE)?;
    let example = builtin::SOURCES
        .first()
        .map(|(_, t)| *t)
        .unwrap_or_default();
    std::fs::write(dir.join(author::EXAMPLE_FILE), example)?;
    std::fs::write(dir.join(author::DRAFT_FILE), base.unwrap_or_default())?;
    if !references.is_empty() {
        let into = dir.join(author::REFERENCES_DIR);
        std::fs::create_dir_all(&into)?;
        for file in references {
            let Some(file_name) = file.file_name() else {
                continue;
            };
            if file.is_dir() {
                copy_dir(file, &into.join(file_name))?;
            } else {
                std::fs::copy(file, into.join(file_name))?;
            }
        }
    }
    Ok(dir)
}

/// A draft's name from the request: its first words, safe for a folder.
fn draft_name(request: &str, now: i64) -> String {
    let words: String = request
        .chars()
        .filter(|c| !c.is_control())
        .take(16)
        .collect::<String>()
        .trim()
        .to_string();
    let stamp = jiff::Timestamp::from_millisecond(now)
        .map(|t| {
            t.to_zoned(jiff::tz::TimeZone::system())
                .strftime("%m%d-%H%M")
                .to_string()
        })
        .unwrap_or_default();
    if words.is_empty() {
        stamp
    } else {
        format!("{stamp} {words}")
    }
}

/// Lessons kept from earlier runs of template `id`, given to new runs.
pub fn experience_file(id: &str) -> Option<PathBuf> {
    Some(user_dir()?.join(format!("{id}.experience.md")))
}

pub fn experience(id: &str) -> Option<String> {
    std::fs::read_to_string(experience_file(id)?)
        .ok()
        .filter(|t| !t.trim().is_empty())
}

/// The "lessons worth keeping" section of a retrospective: what follows a
/// heading naming it, up to the next heading of the same or a higher level.
fn kept_lessons(retro: &str) -> Option<String> {
    let mut lines = retro.lines();
    let mut level = 0;
    for line in lines.by_ref() {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#')
            && (trimmed.contains("建议保留的经验")
                || trimmed.to_lowercase().contains("lessons worth keeping"))
        {
            level = trimmed.chars().take_while(|c| *c == '#').count();
            break;
        }
    }
    if level == 0 {
        return None;
    }
    let mut out = Vec::new();
    for line in lines {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') && trimmed.chars().take_while(|c| *c == '#').count() <= level {
            break;
        }
        out.push(line);
    }
    let text = out.join("\n").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn run_file(dir: &Path) -> PathBuf {
    dir.join(".atrium").join("run.json")
}

/// Writes through a temporary file, so a crash never leaves half a run.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

pub fn load_run(dir: &Path) -> Option<Run> {
    Run::from_json(&std::fs::read_to_string(run_file(dir)).ok()?).ok()
}

pub(super) fn save_run(dir: &Path, run: &Run) -> std::io::Result<()> {
    write_atomic(&run_file(dir), &run.to_json())
}

pub(super) fn append_log(dir: &Path, event: serde_json::Value) {
    use std::io::Write as _;
    let path = dir.join(".atrium").join("log.jsonl");
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{event}");
    }
}

/// A folder name that is free under `parent`.
fn free_dir(parent: &Path, name: &str) -> PathBuf {
    let base: String = name
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let mut dir = parent.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = parent.join(format!("{base}-{n}"));
        n += 1;
    }
    dir
}

/// Copies `files` into `<dir>/inputs/`, renaming on clashes; their paths
/// relative to `dir`, one per line.
fn copy_inputs(dir: &Path, files: &[PathBuf]) -> std::io::Result<String> {
    let inputs = dir.join("inputs");
    std::fs::create_dir_all(&inputs)?;
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
            copy_dir(file, &target)?;
        } else {
            std::fs::copy(file, &target)?;
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
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

impl Workspace {
    /// Built-in templates, then the user's; a user's template with a
    /// built-in id takes its place.
    pub fn workflow_templates(&self) -> Vec<TemplateEntry> {
        let mut out: Vec<TemplateEntry> = builtin::all()
            .into_iter()
            .map(|template| TemplateEntry {
                template,
                source: TemplateSource::Builtin,
            })
            .collect();
        if let Some(dir) = user_dir()
            && let Ok(entries) = std::fs::read_dir(&dir)
        {
            let mut files: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "toml"))
                .collect();
            files.sort();
            for path in files {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                match Template::parse(&text) {
                    Ok(template) => {
                        out.retain(|e| e.template.id != template.id);
                        out.push(TemplateEntry {
                            template,
                            source: TemplateSource::User(path),
                        });
                    }
                    Err(error) => log::warn!("workflow template {}: {error}", path.display()),
                }
            }
        }
        let folders: Vec<PathBuf> = self
            .settings
            .recent_folders
            .iter()
            .chain(&self.settings.projects)
            .map(|folder| folder.join(".atrium").join("workflows"))
            .filter(|dir| dir.is_dir())
            .collect();
        for dir in folders {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut files: Vec<PathBuf> = entries
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == "toml"))
                .collect();
            files.sort();
            for path in files {
                let Some(template) = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|text| Template::parse(&text).ok())
                else {
                    continue;
                };
                out.retain(|entry| entry.template.id != template.id);
                out.push(TemplateEntry {
                    template,
                    source: TemplateSource::Folder(path),
                });
            }
        }
        out
    }

    /// Runs read from their folders, newest first; folders that are gone
    /// drop out of the list.
    pub fn refresh_workflows(&mut self) {
        let dirs = self.settings.workflow_runs.clone();
        let mut runs = Vec::new();
        for dir in &dirs {
            if let Some(run) = load_run(dir) {
                runs.push(RunEntry {
                    dir: dir.clone(),
                    run,
                });
            }
        }
        runs.sort_by_key(|r| std::cmp::Reverse(r.run.created_ms));
        if runs.len() != dirs.len() {
            self.settings.workflow_runs = runs.iter().map(|r| r.dir.clone()).collect();
            self.settings.save();
        }
        self.workflow_runs = runs;
    }

    pub fn enter_workflows(&mut self, run: Option<PathBuf>, cx: &mut Context<Self>) {
        self.nav += 1;
        self.refresh_workflows();
        self.room = Room::Workflows { run };
        self.authoring = None;
        self.template_editing = None;
        cx.notify();
    }

    /// Shows the visual editor on the template `id`, or on a new one.
    pub fn open_editor(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        self.enter_workflows(None, cx);
        self.template_editing = Some(Editing {
            id,
            nonce: self.nav,
        });
    }

    /// The template `id` as text, as its file holds it, with where it
    /// came from.
    pub fn template_source(&self, id: &str) -> Option<(Template, TemplateSource, String)> {
        let entry = self
            .workflow_templates()
            .into_iter()
            .find(|e| e.template.id == id)?;
        let text = match &entry.source {
            TemplateSource::User(path) | TemplateSource::Folder(path) => {
                std::fs::read_to_string(path).unwrap_or_else(|_| entry.template.to_toml())
            }
            TemplateSource::Builtin => builtin::SOURCES
                .iter()
                .find(|(i, _)| *i == id)
                .map(|(_, t)| t.to_string())
                .unwrap_or_else(|| entry.template.to_toml()),
        };
        Some((entry.template, entry.source, text))
    }

    /// Saves a template from the visual editor. `was` is the id it was
    /// opened with: the user's file under that id is renamed with it.
    pub fn save_template(
        &mut self,
        template: &Template,
        was: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        let text = template.to_toml();
        let report = author::inspect(&text, AGENT_IDS, lang());
        if let Some(error) = report.error {
            cx.emit(Notice::Error(error));
            return;
        }
        let templates = self.workflow_templates();
        let source_of = |id: &str| {
            templates
                .iter()
                .find(|e| e.template.id == id)
                .map(|e| e.source.clone())
        };
        let old = was.and_then(source_of);
        let taken = source_of(&template.id);
        if was != Some(template.id.as_str())
            && matches!(
                taken,
                Some(TemplateSource::User(_) | TemplateSource::Folder(_))
            )
        {
            cx.emit(Notice::Error(tf(
                "Another template is already called {id}.",
                &[("id", &template.id)],
            )));
            return;
        }
        let path = match (&old, was == Some(template.id.as_str())) {
            (Some(TemplateSource::User(path) | TemplateSource::Folder(path)), true) => path.clone(),
            (Some(TemplateSource::Folder(path)), false) => {
                path.with_file_name(format!("{}.toml", template.id))
            }
            _ => match user_dir() {
                Some(d) => d.join(format!("{}.toml", template.id)),
                None => return,
            },
        };
        let written = path
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| write_atomic(&path, &text));
        if let Err(error) = written {
            cx.emit(Notice::Error(error.to_string()));
            return;
        }
        // A renamed template leaves no copy under its old id.
        if let Some(TemplateSource::User(old) | TemplateSource::Folder(old)) = &old
            && *old != path
        {
            let _ = std::fs::remove_file(old);
        }
        let replaces_builtin = matches!(taken, Some(TemplateSource::Builtin));
        cx.emit(Notice::Success(if replaces_builtin {
            tf(
                "Saved as {name}; it takes the built-in one's place.",
                &[("name", &template.name)],
            )
        } else {
            tf("Saved as {name}.", &[("name", &template.name)])
        }));
        self.pick_template = Some(template.id.clone());
        self.enter_workflows(None, cx);
    }

    /// Shows the form for writing a template with an agent.
    pub fn open_authoring(&mut self, base: Option<String>, cx: &mut Context<Self>) {
        self.enter_workflows(None, cx);
        self.authoring = Some(Authoring { base });
    }

    pub fn workflow_run(&self, dir: &Path) -> Option<&RunEntry> {
        self.workflow_runs.iter().find(|r| r.dir == dir)
    }

    /// Where new runs go unless the user picks a folder.
    pub fn workflow_home(&self) -> PathBuf {
        dirs::home_dir()
            .unwrap_or_default()
            .join("Atrium")
            .join(t("Workflows"))
    }

    /// Starts a run of `template_id`: a new folder with the files, the
    /// template's snapshot and the run's state. The runner picks it up.
    pub fn start_run(
        &mut self,
        template_id: &str,
        mut values: HashMap<String, String>,
        files: HashMap<String, Vec<PathBuf>>,
        skip: Vec<String>,
        cx: &mut Context<Self>,
    ) -> Option<PathBuf> {
        let entry = self
            .workflow_templates()
            .into_iter()
            .find(|e| e.template.id == template_id)?;
        let template = entry.template;
        let has_commands = template.steps.iter().any(|step| {
            step.checks
                .iter()
                .any(|check| matches!(&check.check, atrium_workflow::Check::Command { .. }))
        });
        if has_commands
            && matches!(&entry.source, TemplateSource::Folder(_))
            && self.settings.workflow_confirmed.get(&template.id) != Some(&template.version)
        {
            cx.emit(Notice::Error(tf(
                "Review this template's commands and confirm it before starting {name}.",
                &[("name", &template.name)],
            )));
            return None;
        }
        // A dry run first, so a missing answer says so before any folder exists.
        let mut check = values.clone();
        for (key, list) in &files {
            if !list.is_empty() {
                check.insert(key.clone(), "-".into());
            }
        }
        if let Err(error) = Run::new(template.clone(), check, &skip, 0) {
            cx.emit(Notice::Error(match error {
                atrium_workflow::run::RunError::MissingInput(label) => {
                    tf("Fill in {label} first.", &[("label", &label)])
                }
                other => other.to_string(),
            }));
            return None;
        }
        let stamp = jiff::Zoned::now().strftime("%Y%m%d-%H%M").to_string();
        let home = self.workflow_home();
        let dir = free_dir(&home, &format!("{}-{stamp}", template.name));
        let made = (|| -> std::io::Result<Run> {
            std::fs::create_dir_all(dir.join(".atrium"))?;
            for input in &template.inputs {
                if input.kind == InputKind::Files {
                    let list = files.get(&input.key).cloned().unwrap_or_default();
                    values.insert(input.key.clone(), copy_inputs(&dir, &list)?);
                }
            }
            std::fs::write(
                dir.join(".atrium").join("workflow.toml"),
                template.to_toml(),
            )?;
            let mut run = Run::new(template.clone(), values, &skip, now_ms())
                .map_err(|e| std::io::Error::other(e.to_string()))?;
            run.experience = experience(&template.id);
            save_run(&dir, &run)?;
            append_log(
                &dir,
                serde_json::json!({
                    "at": run.created_ms,
                    "event": "run_started",
                    "template": template.id,
                    "version": template.version
                }),
            );
            Ok(run)
        })();
        match made {
            Ok(_) => {
                self.settings.workflow_runs.insert(0, dir.clone());
                self.settings.save();
                self.enter_workflows(Some(dir.clone()), cx);
                cx.emit(Notice::Success(tf(
                    "Started {name}.",
                    &[("name", &template.name)],
                )));
                Some(dir)
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&dir);
                cx.emit(Notice::Error(tf(
                    "Could not start the workflow: {error}",
                    &[("error", &error)],
                )));
                None
            }
        }
    }

    /// Changes a run on disk through `change`, then shows the result.
    fn change_run(
        &mut self,
        dir: &Path,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Run, i64) -> Result<(), String>,
    ) {
        let Some(mut run) = load_run(dir) else {
            cx.emit(Notice::Error(t("This run's files are gone.").into()));
            return;
        };
        if let Err(error) = change(&mut run, now_ms()) {
            cx.emit(Notice::Error(error));
            return;
        }
        if let Err(error) = save_run(dir, &run) {
            cx.emit(Notice::Error(error.to_string()));
            return;
        }
        append_log(
            dir,
            serde_json::json!({"at": now_ms(), "event": "run_changed", "state": run.state}),
        );
        self.refresh_workflows();
        cx.notify();
    }

    /// Lets the run go on past a stop, with the option picked there (by
    /// id, from the step's options file) and a note for later steps.
    pub fn approve_step(
        &mut self,
        dir: &Path,
        step: &str,
        choice: Option<String>,
        note: &str,
        cx: &mut Context<Self>,
    ) {
        let picked = choice.and_then(|id| {
            let run = load_run(dir)?;
            let file = run.options_file(step)?;
            let text = std::fs::read_to_string(dir.join(file)).ok()?;
            atrium_workflow::read_choices(&text)
                .into_iter()
                .find(|c| c.id == id)
        });
        self.change_run(dir, cx, |run, now| {
            run.approve(step, picked, note, now).map_err(|e| match e {
                atrium_workflow::run::RunError::ChoiceNeeded(_) => {
                    t("Pick one of the options first.").to_string()
                }
                other => other.to_string(),
            })
        });
    }

    pub fn redo_step(&mut self, dir: &Path, step: &str, note: &str, cx: &mut Context<Self>) {
        self.change_run(dir, cx, |run, now| {
            run.redo(step, note, now).map_err(|e| e.to_string())
        });
    }

    pub fn skip_step(&mut self, dir: &Path, step: &str, cx: &mut Context<Self>) {
        self.change_run(dir, cx, |run, now| {
            run.skip(step, now).map_err(|e| e.to_string())
        });
    }

    pub fn pause_run(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.change_run(dir, cx, |run, now| {
            run.pause(now);
            Ok(())
        });
    }

    pub fn resume_run(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.change_run(dir, cx, |run, now| {
            run.resume(now);
            Ok(())
        });
    }

    pub fn stop_run(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let threads: Vec<String> = load_run(dir)
            .map(|run| {
                run.steps
                    .iter()
                    .filter(|step| step.status == atrium_workflow::run::StepStatus::Running)
                    .filter_map(|step| step.thread().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        self.change_run(dir, cx, |run, now| {
            run.stop(now);
            Ok(())
        });
        for thread in threads {
            self.workflow_interrupt_thread(&thread, cx);
        }
    }

    /// Keeps a finished run's retrospective lessons for the next runs of
    /// its template (appended, dated, to the template's experience file).
    pub fn keep_lessons(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Some(run) = load_run(dir) else { return };
        let lessons = std::fs::read_to_string(dir.join("reports").join("RETRO.md"))
            .ok()
            .and_then(|t| kept_lessons(&t));
        let Some(lessons) = lessons else {
            cx.emit(Notice::Error(
                t("The retrospective has no lessons worth keeping section.").into(),
            ));
            return;
        };
        let Some(file) = experience_file(&run.template.id) else {
            return;
        };
        let title = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let mut text = std::fs::read_to_string(&file).unwrap_or_default();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&format!("\n## {title}\n\n{lessons}\n"));
        let written = file
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| std::fs::write(&file, text));
        match written {
            Ok(()) => cx.emit(Notice::Success(tf(
                "Kept. The next {name} runs start with these lessons; edit them in {path}.",
                &[("name", &run.template.name), ("path", &file.display())],
            ))),
            Err(error) => cx.emit(Notice::Error(error.to_string())),
        }
        cx.notify();
    }

    /// Starts writing a template with `backend`'s agent: a draft folder,
    /// then a chat in it with the request, shown at once. `base` is the
    /// template to change, when one is.
    pub fn start_authoring(
        &mut self,
        request: String,
        base: Option<String>,
        backend: String,
        references: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        if request.trim().is_empty() {
            cx.emit(Notice::Error(
                t("Say what the workflow should do first.").into(),
            ));
            return;
        }
        let Some(parent) = drafts_dir() else { return };
        let base = base.and_then(|id| {
            self.workflow_templates()
                .into_iter()
                .find(|e| e.template.id == id)
        });
        let base_text = base.as_ref().map(|e| match &e.source {
            TemplateSource::User(path) | TemplateSource::Folder(path) => {
                std::fs::read_to_string(path).unwrap_or_else(|_| e.template.to_toml())
            }
            TemplateSource::Builtin => builtin::SOURCES
                .iter()
                .find(|(i, _)| *i == e.template.id)
                .map(|(_, t)| t.to_string())
                .unwrap_or_else(|| e.template.to_toml()),
        });
        let name = draft_name(&request, now_ms());
        let dir = match make_draft(&parent, &name, base_text.as_deref(), &references) {
            Ok(dir) => dir,
            Err(error) => {
                cx.emit(Notice::Error(error.to_string()));
                return;
            }
        };
        let text = author::brief(
            &request,
            base.as_ref().map(|e| &e.template),
            !references.is_empty(),
            lang(),
        );
        self.authoring = None;
        self.start_chat_in(text, backend, dir, true, cx);
    }

    /// Saves a draft as one of the user's templates: over the user's own
    /// template with its id when there is one, else as `<id>.toml`.
    pub fn save_draft(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let Ok(text) = std::fs::read_to_string(dir.join(author::DRAFT_FILE)) else {
            return;
        };
        let report = author::inspect(&text, AGENT_IDS, lang());
        let Some(template) = report.template else {
            cx.emit(Notice::Error(
                report
                    .error
                    .unwrap_or_else(|| t("The draft does not read yet.").into()),
            ));
            return;
        };
        let existing = self
            .workflow_templates()
            .into_iter()
            .find(|e| e.template.id == template.id);
        let replaces_builtin = matches!(
            existing.as_ref().map(|e| &e.source),
            Some(TemplateSource::Builtin)
        );
        let path = match existing.map(|e| e.source) {
            Some(TemplateSource::User(path)) => path,
            _ => match user_dir() {
                Some(d) => d.join(format!("{}.toml", template.id)),
                None => return,
            },
        };
        let written = path
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| write_atomic(&path, &text));
        match written {
            Ok(()) => {
                cx.emit(Notice::Success(if replaces_builtin {
                    tf(
                        "Saved as {name}; it takes the built-in one's place.",
                        &[("name", &template.name)],
                    )
                } else {
                    tf("Saved as {name}.", &[("name", &template.name)])
                }));
                self.pick_template = Some(template.id);
                self.enter_workflows(None, cx);
            }
            Err(error) => cx.emit(Notice::Error(error.to_string())),
        }
    }

    /// Drops a run from the list; its folder stays.
    pub fn forget_run(&mut self, dir: &Path, cx: &mut Context<Self>) {
        self.settings.workflow_runs.retain(|d| d != dir);
        self.settings.save();
        self.refresh_workflows();
        if matches!(&self.room, Room::Workflows { run: Some(d) } if d == dir) {
            self.room = Room::Workflows { run: None };
        }
        cx.notify();
    }

    /// Records that the user has seen a folder template's commands.
    pub fn confirm_workflow_template(&mut self, id: &str, version: u32, cx: &mut Context<Self>) {
        self.settings
            .workflow_confirmed
            .insert(id.to_string(), version);
        self.settings.save();
        cx.notify();
    }

    pub fn workflow_environment(&self, template: &Template) -> Vec<atrium_workflow::runner::Need> {
        atrium_workflow::runner::environment_needs(template)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_lessons_section_is_taken_from_a_retrospective() {
        let retro = "# 复盘\n## 弯路\n- 第二天才清洗数据\n## 建议保留的经验\n- 数据先清洗再建模\n- 每问先做基线\n### 细节\n- 图用 PDF\n## 时间\n- 写作太晚\n";
        assert_eq!(
            kept_lessons(retro).as_deref(),
            Some("- 数据先清洗再建模\n- 每问先做基线\n### 细节\n- 图用 PDF")
        );
        assert_eq!(kept_lessons("# 复盘\n没有那一节"), None);
    }

    #[test]
    fn inputs_are_copied_without_clobbering() {
        let src = tempfile::tempdir().unwrap();
        let a = src.path().join("A.pdf");
        std::fs::write(&a, b"%PDF").unwrap();
        let data = src.path().join("data");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("x.csv"), b"1,2").unwrap();
        let run = tempfile::tempdir().unwrap();
        let lines = copy_inputs(run.path(), &[a.clone(), a, data]).unwrap();
        assert_eq!(lines, "inputs/A.pdf\ninputs/A-2.pdf\ninputs/data");
        assert!(run.path().join("inputs/data/x.csv").exists());
        assert_eq!(
            free_dir(run.path(), "a/b"),
            run.path().join("a-b"),
            "names are made safe for a folder"
        );
    }

    #[test]
    fn a_draft_folder_has_the_guide_the_example_and_the_references() {
        let tmp = tempfile::tempdir().unwrap();
        let reference = tmp.path().join("notes.md");
        std::fs::write(&reference, "参考").unwrap();
        let parent = tmp.path().join("drafts");
        let dir = make_draft(&parent, "1121-0930 周报", Some("id = \"x\""), &[reference]).unwrap();
        assert_eq!(dir.parent(), Some(parent.as_path()));
        assert!(
            std::fs::read_to_string(dir.join("FORMAT.md"))
                .unwrap()
                .contains("[[steps]]")
        );
        assert!(dir.join("example.toml").exists());
        assert_eq!(
            std::fs::read_to_string(dir.join("template.toml")).unwrap(),
            "id = \"x\""
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("references/notes.md")).unwrap(),
            "参考"
        );
        let again = make_draft(&parent, "1121-0930 周报", None, &[]).unwrap();
        assert_ne!(again, dir);
        assert_eq!(
            std::fs::read_to_string(again.join("template.toml")).unwrap(),
            ""
        );
    }

    #[test]
    fn drafts_are_named_after_the_request() {
        let now = jiff::Timestamp::from_second(1_700_000_000)
            .unwrap()
            .as_millisecond();
        let name = draft_name("每周读 arXiv 新论文写一份简报，挑出和我课题相关的", now);
        assert!(name.ends_with(" 每周读 arXiv 新论文写一份"), "{name}");
        assert_eq!(draft_name("  ", now).len(), "1114-2213".len());
    }
}
