# Building the project scheduler

A brief for implementing the scheduler behind Atrium's three-level projects
(controller → leads → sub-agents). The UI and the data model already exist and
are tested; this file says what the scheduler must do, where it plugs in, and
how to know it is done.

## The model in one paragraph

A project is a folder with an `.atrium/` directory. Every agent session in it
is a `Workstream` in `.atrium/project.json` with a `role`: one `controller`
(splits the project, keeps `.atrium/plan.md`, writes no code), several `lead`s
(each owns a part and has a roadmap), and `worker`s (one task each, for one
lead). Agents never call Atrium. They ask for a new agent one level down by
**writing a task file**, `.atrium/tasks/<owner>/<id>.md`, and report by
**editing it** (`status: done` plus a `## Result`). Atrium is the scheduler: it
notices the files, starts the agents, and tells the asker when they finish.

Everything you need to read and write the files is in
`crates/atrium-app/src/project.rs` (tested in its `tests` module):

| Need | Call |
| --- | --- |
| Parse a task file | `Task::parse(owner, id, text)` (done for you by `Project::reload` into `project.tasks`) |
| Tasks that should get an agent now | `Project::startable()`: honours roles, approval and `max_workers` |
| The `Workstream` to create for one | `Project::stream_from_task(&task, backend)`: id, role, parent, scope, goal |
| Mark a task running / failed | `Project::set_task_status(key, TaskStatus::…)` |
| Finished tasks whose parent was not told | `Project::unreported()` |
| Kickoff prompt for any role | `Project::kickoff(&stream)` |
| Post a notice | `Project::post(from, to, text)` |
| Notices a stream has not seen | `Project::unread(id)`, `Project::mark_read(id)` |

Do not change the file formats or these functions' meaning: the UI reads the
same data (`Workspace::stream_views`, `proposed_leads`, `queued_tasks`). Add
functions if you need them, with tests.

## What to build

All of it lives in `crates/atrium-app/src/workspace_project.rs` (the
`Workspace` side of projects). Today:

- `init_projects` starts a 2-second timer calling `tick_project`.
- `tick_project` reloads **only the shown project** and opens chats whose agent
  came up.
- `create_stream(name, role, backend, goal, scope, cx)` builds a `Workstream`,
  saves it, starts its session (Codex: `thread_start` + `goal_set`; ACP:
  `new_session`) and sends `project.kickoff(&stream)`.
- `watch_streams` sees every `AgentEvent` from a project thread (crossings,
  failed turns).
- `approve_task` / `reject_task` (called by the UI) write `approved` in
  `project.json` or mark the task failed.

### 1. Start a stream from a `Workstream`

Split `create_stream` in two:

```rust
pub fn create_stream(&mut self, name, role, backend, goal, scope, cx)  // UI: builds the Workstream, then calls ↓
fn start_stream(&mut self, root: &PathBuf, stream: Workstream, cx)     // saves it, starts the session, sends the kickoff
```

`start_stream` takes `root` explicitly (the project need not be the shown
one) and must not assume `self.project_root()`. On failure keep today's
behaviour (`stream_failed`) and, for a stream with a `task`, also
`set_task_status(task, Failed)` and post to the parent:
`"Could not start an agent for <key>: <error>"`.

### 2. Tick every project

`tick_project` must process **every** project in `self.projects`, not just
the shown one: workers keep running when the user looks elsewhere. For each:

1. `project.reload()` (cheap: it stats the files and returns early when
   nothing changed). Only `cx.notify()` when something changed.
2. **Start agents**: for each `task` in `project.startable()`:
   - backend = `task.agent` if it names a known backend (`codex`, or an ACP
     agent id in `self.agents`, optionally `agent@device`), else the owner's
     backend. A name that is not known: mark the task `failed`, post to the
     owner `"Unknown agent '<name>' in <key>; use codex, claude, deepseek or antigravity."`.
   - If that backend is not ready (Codex client missing, ACP agent not
     `Ready`): skip; try again next tick. Do not mark it failed.
   - `set_task_status(key, Running)` **before** starting the session, so a
     second tick cannot start it twice; then
     `start_stream(root, project.stream_from_task(task, backend))`.
3. **Report finished tasks**: for each `(stream, task)` in
   `project.unreported()`:
   - post from `ATRIUM` to `stream.parent`:
     - done: `"<role label> \"<stream.name>\" finished <task.rel_path()>: <first line of the result>. Read its Result and review its changes."`
     - failed: same with `"could not finish"` and the result's first line (or `"no reason given"`).
   - set `reported = true` on the stream and `save()`.
   - **Wake** the parent (see 4).

### 3. Leads hand in their goal

A lead started from a controller task finishes the same way (its kickoff tells
it to set `status: done` when its whole goal is done). Step 2.3 covers it:
the controller gets the notice and is woken. Nothing special.

### 4. Waking, not polling

A parent is woken when (a) one of its tasks is reported, or (b) a notice
addressed **by id** to the controller arrives (leads post these at
milestones). Notices to `all` do not wake anyone.

To wake stream `s`:

- if its thread is running: do nothing; the notices go with its next message
  (already handled by `with_stream_notices`). Exception: Codex accepts a
  steer while running, so for Codex send it as today's `deliver_notices`
  does.
- if it is idle: call the existing `deliver_notices(&s.id)` logic, but
  parameterised by `root` (today it reads `active_project()`).
- if it is waiting on the user (approval or question): do nothing.
- Debounce: at most one wake per stream per 30 s; collect what arrives in
  between into the same message. Keep the last wake time in memory only.

To detect (b), remember per project the number of notices seen at the last
tick; for new ones whose `to` contains the controller's id and whose `from` is
not the controller, wake the controller.

### 5. Worker cleanup

A worker whose task is reported stays in `project.json` (the UI shows it as
done under its lead) but its session should stop costing anything:

- ACP: nothing to do once its turn ended.
- Codex: if the thread has a goal, `goal_set(thread, None, Some(Complete), None)`
  so Codex stops continuing it.

Do not archive or delete threads; the user may open them.

### 6. Safety limits

- `max_workers` (per lead, default 3) is already enforced by `startable()`.
- Add a project-wide cap of 12 running workstreams (any role, counted from
  `stream_views` states `Working`/`Starting`); over it, leave tasks open.
- Never start an agent for a task whose owner is a `worker` (the model already
  refuses: `Role::child()` is `None`).
- A task file whose owner directory names no workstream is ignored (it may be
  from a removed lead). Log it once.

## Tests

`crates/atrium-app` has no GPUI test harness, so keep the scheduling decisions
in plain functions on `Project` (or a new `scheduler.rs` with no GPUI types)
and test those. Required:

1. A controller task becomes a lead immediately (the default,
   `approve_leads` off), and only after approval when it is on.
2. Two ticks in a row start one agent per task, never two (status is set to
   `running` first).
3. With `max_workers = 2` and 3 open tasks under one lead, two start; when one
   finishes, the third starts on the next tick.
4. A finished task posts exactly one notice to its parent, and only once
   across reloads and restarts (`reported` is saved).
5. A notice `lead → controller` wakes the controller; `lead → all` does not.
6. Unknown `agent:` marks the task failed with a notice; a known but
   offline agent leaves it open.

Then an end-to-end check on Xvfb with the mocks, as the README's testing
section describes (`mock-codex`, `mock-acp --name claude`): create a
controller, write `tasks/controller/web.md` by hand,
see the lead start and receive its kickoff with the phased roadmap (with
`approve_leads` on, only after "Start" in the UI); write
`tasks/web/a.md`, see the worker start; set it to `done` with a result, see
the notice reach the lead and the lead's next turn begin.

## Done when

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` are
  clean.
- The scenario above works with both a Codex and a Claude (ACP) lead.
- Nothing in `ui/` needed to change, except to show new states if you add any.
