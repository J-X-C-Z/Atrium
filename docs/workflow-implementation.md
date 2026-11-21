# Building workflows

A workflow takes a brief and follows a preset path of steps to a finished
result. The first one: a math modeling contest. The user drops in the
problem and its data; agents read and model it, write and run the code,
draw the figures, write the paper in LaTeX, check it against the contest's
rules and send faults back until it passes. The run stops where the user
wants to look first.

Read `AGENTS.md` first. Read `crates/atrium-workflow` next: the model is
built and tested, and this brief is about running it.

## Decisions (made with the user)

1. **Stops at key points.** By default a run stops after the modeling plan
   and after the paper passes its checks. Each template decides where to
   stop (`gate = true` on a step).
2. **LaTeX to PDF.** The math modeling template writes `paper/main.tex` and
   compiles it with XeLaTeX. The side panel shows the PDF.
3. **An agent per step.** Each step can name its agent and effort, with
   defaults for the template. For example, Claude Code models and writes,
   and Codex codes.
4. **Templates are files.** A template is a `workflow.toml`. The UI can
   also edit it. One template ships built in.

The stages of the built-in template follow the user's reference,
[Sulflower314/mathmodel-contest-suite](https://github.com/Sulflower314/mathmodel-contest-suite).
That is a Claude Code plugin with one skill per stage: analysis and
modeling, code and figures, draw.io diagrams, writing, verification, and
review. Its NOTICE says its templates and review materials carry no license
to redistribute. **Do not copy its text, LaTeX templates, rubrics or
scripts into this repository.** The built-in template is written in our
own words. A step can name a skill (`skill = "mathmodel-contest-suite:..."`).
When that step's agent is Claude Code, the prompt tells it to use the
skill if it is installed. That is the only coupling.

Version 2 of the template also follows a second reference,
[Odyphus/MathModel-Copilot](https://github.com/Odyphus/MathModel-Copilot).
Its MIT license covers only what it inherited from upstream, so the same
rule holds: take ideas, not text. What it added:

- **The user decides.** A step lays out candidate routes and the user
  picks one.
- **Per sub-question loop.** Each sub-question is modeled, solved,
  verified and checked for sensitivity on its own.
- **Results move through states.** A result is written, then run, then
  verified, and only verified ones reach the paper.
- **Cross-stage back-check.** A fault goes back to the stage where it
  started.
- **Independent reviewer panel.** Several reviewers each read the paper
  on their own.
- **AI-use record.** What AI did is recorded, and the paper discloses it.
- **Retrospective.** Lessons that carry over to the next contest.

## Status: what exists, what is yours

**Built (in `crates/atrium-workflow`, tested):**

- `Template`: parses and validates `workflow.toml`. It covers inputs
  (files, text, choice, private), defaults, a shared preamble, and steps.
  Each step has `needs`, `outputs`, `checks` (each check may have a
  `when`), `gate`, `optional`, `fixes` (a reviewer sends faults back to the
  step it reviews), `when`, `attempts` and `timeout_min`. Steps may only
  need or fix steps before them, so there are no cycles.
  - **`decide`**: the step writes an options file. The user must pick one
    at its stop, and may add a note. Choices and notes go into every later
    step's prompt.
  - **`each`**: the step repeats once per item an earlier step listed, for
    example once per sub-question. Its items are run steps
    `<step>:<item>`, and `{{item.id}}` and `{{item.title}}` are filled
    into the prompt, the outputs and the checks.
  - **`fixes`** can be a list. A failing result's `send_back` (a step or
    `step:item`) picks which step gets the faults; otherwise the first
    step in the list does.
  - **`verdict`**: a local check of a JSON file the step writes,
    `{"pass", "issues": [{"step", "problem", "fix", "blocking"}]}`. Each
    blocking issue becomes a failed result whose `send_back` is its step.
- `run::Run`: the state machine. Its JSON is the file format of
  `.atrium/run.json`. Its calls are:
  - starting work: `new`, `ready`, `to_expand`, `expand`, `begin`,
    `set_thread`;
  - finishing it: `turn_finished`, then `checked` (returning `Outcome`);
  - the user's decisions: `approve(id, choice, note)`, `redo`, `skip`;
  - the run as a whole: `pause`, `resume`, `stop`, `recover`.

  A run also keeps `decisions` (the user's picks and notes) and
  `experience` (lessons kept from earlier runs, which go into every
  prompt).
- `check::local_results`: the checks that only read files (`exists`,
  `contains`, `absent`, plus every step's `outputs`). Private input values
  (names that must not appear) are matched but never echoed.
- `render::prompt`: a step's full prompt. It includes the preamble, the
  step with inputs filled in (never private ones), the files to leave, the
  checks in words, the reasons from the last try, and the skill hint.
- `builtin::all()`: the math modeling template (`builtin/mathmodel.toml`).

**Built in the app (UI side):**

- `workspace_workflows.rs` handles templates and runs on disk:
  - **Templates:** the built-in and the user's templates
    (`workflow_templates`, `copy_template`).
  - **Runs:** `start_run` makes the run folder, copies the files into
    `inputs/`, and writes the snapshot and `run.json`. It also keeps the
    run list in settings (`workflow_runs`, `refresh_workflows`).
  - **Your decisions on a run:** `approve_step`, `redo_step`,
    `skip_step`, `pause_run`, `resume_run`, `stop_run` and `forget_run`.
    Each loads the run, changes it and saves it atomically.
- The **Workflows room** (`ui/workflow.rs`):
  - The start form has a drop zone for files, choices, and private
    inputs.
  - Optional steps can be switched off.
  - The run view is a timeline of steps. For each step it shows its
    checks, its files (opened in the preview panel), the exact prompt, its
    conversation, and the buttons approve, ask for changes, retry and skip.
- `ui::workflow::RUNNER_READY` is `false` until the runner lands. While
  it is false, the run view says so. Set it to `true` in your PR.

**Yours:**

1. The **runner**: it drives runs, starts agents, waits, checks and
   saves.
2. The **runner's checks**: `command`, `pdf_pages` and `review`.
3. **Storage**: the app already reads and writes templates and runs
   (above). Add folder templates (`<folder>/.atrium/workflows/`), with the
   command confirmation in section 3, and the `.atrium/log.jsonl` event
   log.
4. **The TeX environment**: detect it and say what is missing.

The UI (the Workflows room, the template editor, the start dialog, the run
view and the stops) is built on the UI side against the API in section 5.

## 1. Where things live

- **Templates.**
  - Built in: `atrium_workflow::builtin`.
  - The user's: `<config>/Atrium/workflows/<id>.toml`.
  - A folder's: `<folder>/.atrium/workflows/<id>.toml`.
  - Same id: the folder's wins over the user's, which wins over built-in.
  - Copying a built-in template writes it to the user's folder.
- **Runs.** A run is a folder. The default is
  `~/Atrium/工作流/<template name>-<yyyymmdd-HHMM>/`; the user can pick
  another. Inside it:
  - `.atrium/workflow.toml`: the template as it was when the run started.
    Edits to the template never change a run under way.
  - `.atrium/run.json`: `Run::to_json`, written atomically (temp file
    plus rename) after every change.
  - `.atrium/log.jsonl`: one line per event (step started, turn ended,
    check results, the user's decisions), for the run's history view.
  - `inputs/`: the files the user gave. A `files` input's value is their
    paths relative to the run folder, one per line.
  - Everything the steps make, at the paths the template says.
- **The list of runs.** Remember run folders in settings
  (`workflow_runs: Vec<PathBuf>`). Drop a folder from the list when it is
  gone. Never delete a run folder unless the user asks.

## 2. The runner

One per open Atrium, ticking every run whose state is `Running` or
`Waiting`. A tick does the following:

0. For each `(step, file)` in `run.to_expand()`, read `file`
   (`atrium_workflow::read_choices`) and call `run.expand(step, items)`.
   If the list is missing or empty, `expand` fails the step and the run
   waits for the user. Say so in the step's result.
1. For each step in `run.ready()`, up to **two steps at a time per run**
   (the items of a repeating step are ready side by side):
   - Pick the agent: `Template::agent_of(step)`. If none is set, use the
     default chat agent. If that agent is not installed or not running,
     the step does not start. Instead the run gets a notice ("Claude Code
     is not installed"), and the UI shows it on the step.
   - Start a **new conversation** for each try, in the run folder, with
     the step's effort and the access mode "auto". Why a new conversation:
     a try's context should be the files, not a long chat; the reasons
     from the last try are in the prompt.
   - Send `render::prompt(&run, id, skills, lang)`, where `skills` is true
     for Claude Code and `lang` follows the interface language. Then call
     `run.begin`, `run.set_thread`, and save.
2. When that conversation's turn ends, call `run.turn_finished`, then run
   the checks:
   - `check::local_results`;
   - then each runner check from `run.checks_of(id)` in order;
   - then `run.checked(id, results)`.

   Act on the `Outcome`:
   - `Done`, `Retry`, `SentBack`: go on ticking. A retry and a fix start
     new conversations.
   - `Review` (a stop) and `Failed`: notify the user (system notification
     and the chat list's waiting dot).
3. **Turn errors.**
   - A turn that fails (the agent errored or was interrupted) is a failed
     try with one result: "the agent stopped: <reason>".
   - A turn with no output for the step's `timeout_min` is interrupted
     and counts the same way.
4. **Approvals.** Steps run in "auto" mode inside the run folder.
   - An approval request outside the folder waits for the user like any
     other, and the run shows "waiting for your approval" on that step.
   - Under the project autopilot's "unattended" switch for workflows
     (`unattended` on the run), approvals are declined with the message
     "stay inside the run folder", as the autopilot does today.
5. **Restart.**
   - On start, `run.recover()` every listed run.
   - A step whose conversation is still running in the agent (Codex keeps
     it) is re-attached rather than restarted, when the thread can be
     found. Otherwise it starts again.
6. **Pause, resume, stop.** These come from the UI.
   - Pause lets running steps finish and starts nothing new.
   - Stop interrupts running steps and marks the run stopped.
7. **Multi-device.**
   - A run belongs to the device whose folder it is in.
   - Other devices see it through the run list in the overlay (path and
     state only) and can open its conversations as remote chats.
   - Only the owner runs it. It is not moved in this brief.

## 3. The runner's checks

- **`command`.**
  - Run `run` with `sh -c` (Windows: `cmd /C`) in `cwd` (relative to the
    run folder, default the run folder), with the run folder's
    environment.
  - Kill it after `timeout_min`. It passes on exit 0.
  - On failure, the detail is the last 40 lines of its output, trimmed to
    4 KB, so the agent sees the actual error (for LaTeX, the error and the
    line).
  - Never run a command from a template the user has not seen. Templates
    from a folder are shown with their commands before the first run of
    them, and the user confirms once per template version.
- **`pdf_pages`.**
  - Count pages with `hayro` (already planned for previews) or a minimal
    trailer reader.
  - A file that is not a PDF fails with "not a PDF".
  - The detail says the count ("has 33 pages").
- **`review`.**
  - Start a separate conversation with the check's `agent` (default: the
    step's agent) in the run folder, in read-only mode.
  - Give it the rubric and the files, and ask for a JSON verdict
    `{"pass": bool, "reasons": [..]}` in its last message.
  - Parse the last JSON object in the answer. Malformed output counts as a
    failure with detail "the reviewer did not give a verdict", and is
    retried once before it counts.
  - The reasons become the result's detail.
  - An optional `"send_back_to"` in the verdict sets the result's
    `send_back`.
  - **A panel is just several `review` checks on one step.**
    - Run them as **separate conversations that never see each other's
      answers**, at the same time.
    - Each gives its own result; any failure fails the step.

## 3a. Experience

- **Where lessons live:**
  `<config>/Atrium/workflows/<template id>.experience.md`.
- **Keeping lessons:** the run view's "Keep the lessons" button appends
  the "建议保留的经验" section of a run's `reports/RETRO.md`. This is
  built.
- **Using them:** `start_run` copies the file into `run.experience`.
  This is also built.
- **Nothing to add in the runner:** the prompts already carry the
  lessons.

## 4. The TeX environment

The writing step needs `xelatex`, plus Chinese fonts for a Chinese paper.

- **Before starting a run** whose steps have a `command` check mentioning
  `xelatex`, `latexmk` or `typst`, look the tool up on PATH. Also check the
  usual install places: TeX Live, MacTeX `/Library/TeX/texbin`, MiKTeX.
  When it is missing, the start dialog says so, links the installer and
  offers "start anyway" (the run stops at writing).
- **Chinese fonts.** On a Chinese paper, check that `fc-list :lang=zh` (or
  the platform's equivalent) lists at least one font. If not, say which
  to install.
- **Expose** this as `workflow_environment() -> Vec<Need>` with
  `{ tool, found: bool, hint }`. The UI shows it.

## 5. API for the UI (on `Workspace`, in `workspace_workflows.rs`; most of it exists, keep its names)

```rust
pub struct TemplateEntry { pub template: Template, pub source: Source, pub path: Option<PathBuf> }
pub enum Source { Builtin, User, Folder(PathBuf) }

fn workflow_templates(&self) -> Vec<TemplateEntry>;
fn save_template(&mut self, template: &Template, cx) -> Result<PathBuf, String>; // validates
fn copy_template(&mut self, id: &str, cx) -> Result<PathBuf, String>;           // builtin → user
fn delete_template(&mut self, id: &str, cx);                                     // user's only

fn workflow_runs(&self) -> Vec<RunEntry>;   // { dir, run: Run, live: HashMap<step, thread> }
fn start_run(&mut self, template_id: &str, inputs: HashMap<String, String>,
             files: Vec<PathBuf>, skip: Vec<String>, dir: Option<PathBuf>, cx) -> Result<PathBuf, String>;
fn approve_step(&mut self, dir: &Path, step: &str, choice: Option<String>, note: &str, cx);
fn redo_step(&mut self, dir: &Path, step: &str, note: &str, cx);
fn skip_step(&mut self, dir: &Path, step: &str, cx);
fn pause_run / resume_run / stop_run (&mut self, dir: &Path, cx);
fn forget_run(&mut self, dir: &Path, cx);   // drops it from the list; files stay
fn workflow_environment(&self, template: &Template) -> Vec<Need>;
```

- `start_run` does the following:
  - copies `files` into `inputs/`, renaming on clashes, and sets the
    `files` input to their paths;
  - writes `.atrium/workflow.toml` and `run.json`;
  - adds the run to the list and ticks.
- Every change calls `cx.notify()`.
- Each step's conversations stay ordinary chats, tagged with the run, so
  the UI can open them and the chat list can hide them under the run.

## 6. Tests

**In `atrium-workflow`.** The state machine and local checks are covered.
Add to them as you change them.

**Runner tests** (in the app crate's test helpers, with `mock-codex` and
`mock-acp`):

1. A three-step template whose steps write a file through the mock's
   `edit` path:
   - the run starts each step in a new conversation;
   - checks pass;
   - the run ends `Done`.
2. A failing check (`exists` on a file the mock does not write):
   - the second try's prompt holds the reason;
   - after the allowance the run waits, and `redo` with a note starts a
     try whose prompt holds the note.
3. `fixes`: a reviewer step's failing `contains` sends the writer back;
   the writer runs, then the reviewer passes.
4. A gate stops the run; `approve` moves on.
5. `command` check:
   - `exit 1` with output fails, with the output in the detail;
   - a sleep past the timeout is killed and fails.
6. `pdf_pages` on a 2-page PDF fixture with `max = 1` fails, saying "has 2
   pages".
7. Restart: kill the app mid-step; on start the step tries again, and its
   try count is unchanged.
8. Private inputs: a run with `identity = "张三"`:
   - no prompt sent to the mock contains it (assert on the mock's request
     log);
   - an `absent` check finds it in a file.

9. **Repeating step.**
   - A step writes `reports/subproblems.json` with three items;
   - the repeating step expands into three item steps, which start side by
     side (up to the limit);
   - the step after them starts only when all three pass.
10. **Decision.**
    - A deciding step stops;
    - `approve` without a choice is refused;
    - with a choice and a note, the next step's prompt (on the mock's
      request log) holds both.
11. **Verdict send-back.** A verifier writes `verify.json` naming
    `solve:Q2`:
    - only that item and the steps after it go again;
    - Q1 keeps its result.
12. **Panel.** Three `review` checks on one step run as three conversations
    at once; one failing sends the step's `fixes` target back.

**Once, by hand, recorded in `docs/backend-acceptance-matrix.md`:** run the
built-in template on a real past CUMCM problem with real Codex and Claude
Code, on a machine with TeX Live. Record:

- how long each step took;
- how many tries each step needed;
- whether the PDF compiled;
- what the verifier found.

## Done when

- The tests above pass.
- The real run is recorded.
- Starting the built-in template from the UI (once the UI lands) gets to
  the first stop with no manual help.
