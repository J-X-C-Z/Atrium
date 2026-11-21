//! Workflows on the workspace: the templates to pick from, the runs and
//! what the user decides at their stops. This side keeps the files (the
//! run folder, its template snapshot and `run.json`); the runner that
//! starts agents on ready steps and checks their work is the backend's
//! (`docs/workflow-implementation.md`) and drives the same files.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use atrium_workflow::{InputKind, Template, builtin, run::Run};
use gpui::Context;

use super::{Notice, Room, Workspace};
use crate::i18n::{t, tf};

/// Where a template comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TemplateSource {
    Builtin,
    User(PathBuf),
}

#[derive(Clone, Debug)]
pub struct TemplateEntry {
    pub template: Template,
    pub source: TemplateSource,
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

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn user_dir() -> Option<PathBuf> {
    Some(dirs::config_dir()?.join("Atrium").join("workflows"))
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

fn save_run(dir: &Path, run: &Run) -> std::io::Result<()> {
    write_atomic(&run_file(dir), &run.to_json())
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
        let Some(dir) = user_dir() else { return out };
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return out;
        };
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
        out
    }

    /// Copies a template to the user's folder, to change it there.
    pub fn copy_template(&mut self, id: &str, cx: &mut Context<Self>) -> Option<PathBuf> {
        let entry = self
            .workflow_templates()
            .into_iter()
            .find(|e| e.template.id == id)?;
        if let TemplateSource::User(path) = entry.source {
            return Some(path);
        }
        let dir = user_dir()?;
        let path = dir.join(format!("{id}.toml"));
        let text = builtin::SOURCES
            .iter()
            .find(|(i, _)| *i == id)
            .map(|(_, t)| t.to_string())
            .unwrap_or_else(|| entry.template.to_toml());
        let written = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, text));
        match written {
            Ok(()) => {
                cx.emit(Notice::Success(tf(
                    "Copied to {path}. Changes there take the built-in one's place.",
                    &[("path", &path.display())],
                )));
                cx.notify();
                Some(path)
            }
            Err(error) => {
                cx.emit(Notice::Error(error.to_string()));
                None
            }
        }
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
        cx.notify();
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
        self.change_run(dir, cx, |run, now| {
            run.stop(now);
            Ok(())
        });
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
}
