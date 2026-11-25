# Atrium for Orca — design

An Orca plugin that brings Atrium's **projects** (controller → leads → workers,
milestones, task files, task pool, team roster with stand-ins, autopilot) and
**workflows** (templates, runs, gates, send-back, per-item steps, experience,
CUMCM template) into [Orca](https://github.com/stablyai/orca), with a visual
interface for managing them.

The plugin reads and writes the **same files as the Atrium app** (`.atrium/`
in a project, `~/.config/Atrium/workflows/`), so a project can move between the
app and Orca.

Orca's plugin docs are checked out read-only at
`/home/user/stablyai/orca/docs/reference/` (`plugin-authoring.md`,
`plugin-manifest.md`, `plugin-host-api.md`, `plugin-isolated-frames.md`,
`plugin-theme-tokens.md`). Read them; do not copy Orca's source code. Orca is
MIT, but we write our own code against its documented API.

## Layout

```
crates/atrium-orca-core/      Rust cdylib for wasm32-wasip1: JSON ops over
                              atrium-workflow, atrium-team, atrium-project
plugins/orca-atrium/
  orca-plugin.json            manifest (trusted-worker, pluginApi 1, orca >=1.4.259)
  DESIGN.md                   this file
  README.md / README.zh-CN.md
  core/atrium_core.wasm       built artifact (committed; built by scripts/build-core.sh)
  scripts/build-core.sh       cargo build --target wasm32-wasip1 --release + copy
  worker/                     the trusted worker (TypeScript, bundled to worker/dist/main.mjs)
    src/main.ts               activate(orca): wires everything
    src/core.ts               loads the wasm with node:wasi, `call(op, args)`
    src/protocol.ts           SHARED with ui: request/post names and payload types
    src/engine/…              run driver, team scheduler, agent bridge
    src/bridge/…              `atrium` CLI on agents' PATH + unix socket server
  ui/                         panels and editors (TypeScript + Preact, bundled with esbuild)
    src/design/…              design system (tokens from --orca-* vars, components)
    src/sidebar/…             sidebar panel
    src/board/…               project command center (custom editor)
    src/run/…                 run view (custom editor)
    src/template/…            visual template editor (custom editor)
    src/mock/…                mock window.orca host + fixtures for dev and screenshots
    dist/…                    bundled html/js (committed, frames have no network)
  test/                       node tests (worker engine against a fake host)
```

`worker/src/protocol.ts` is the one contract between the worker and the UI.
The UI imports its types (copy at build, or a relative import resolved by esbuild).

Bundling: esbuild from npm (devDependency in `plugins/orca-atrium/package.json`).
Plugin frames have **no network**: every script, style and font must be in the
plugin folder. No CDN. Keep each bundle under 1 MiB if possible (10 MiB hard limit).
Folder limit: 2,000 files, 50 MiB — do not commit `node_modules`.

## Manifest (summary)

- `execution: "trusted-worker"`, `main: "worker/dist/main.mjs"`, `pluginApi: 1`,
  `engines.orca: ">=1.4.259"`.
- Capabilities: `storage:own`, `settings:own`, `workspace:read`, `workspace:write`,
  `workspace:create`, `agent:read`, `agent:launch`, `terminal:read`,
  `terminal:send`, `notifications:show`, `automation:read`, `history:read`.
- `contributes.panels`: `atrium` (html, icon `layers`, title "Atrium") — the sidebar.
- `contributes.customEditors` (all `ui` html, `frame: "isolated"`):
  - `board` — `filenames: ["atrium.board"]`: the **project command center**.
    The worker writes `<project>/.atrium/atrium.board` (a small JSON link file:
    `{ "project": "<abs path>" }`) and keeps it out of git via `.atrium/.gitignore`.
  - `run` — `extensions: [".atrium-run"]`: one **workflow run** view. Link file
    `<run dir>/.atrium/view.atrium-run` → `{ "run": "<abs run dir>" }`.
  - `template` — `extensions: [".workflow.toml"]`, `priority: "option"`: the
    **visual template editor** over the TOML text (it edits the document itself).
- `contributes.newFiles`: "New Atrium workflow template" (`.workflow.toml`).
- `contributes.events`: `agent.status.changed`, `workspace.changed`.
- `contributes.hooks`: `agent.launch.before` (put `atrium` on PATH, tag env),
  `workspace.create.after`.
- `contributes.statusBarItems`: `atrium.needs` — "n need you · m running".
- `contributes.commands`: Open Atrium, New project here, Start workflow…, Open board.
- `contributes.settings`: language (`auto|zh|en`), default agent, max parallel agents,
  autopilot default.

## Core (wasm) — `crates/atrium-orca-core`

Target `wasm32-wasip1`, `crate-type = ["cdylib"]`. Loaded in the worker with
`node:wasi` (preopen the project roots and the Atrium config dir). ABI:

```
alloc(len) -> ptr ; dealloc(ptr,len)
call(ptr,len) -> u64   // input: UTF-8 JSON {"op": "...", "args": {...}}
                       // output: packed (ptr<<32 | len) of JSON {"ok": ...} | {"err": "..."}
```

The core does **no process spawning and no network**: anything that needs a
process (git, check commands, gh) is returned as an *effect* for the worker to
run, or done by the worker directly. File reads/writes inside preopened dirs are
fine (WASI). `atrium-process` needs a `cfg(not(any(unix, windows)))` fallback
returning an "unsupported" error so the crates compile for WASI; change nothing
else in existing crates unless strictly needed, and keep the desktop app's
behavior identical (run the workspace tests).

Ops (args/results are serde JSON of the existing Rust types where possible):

Project
- `project.open {root}` → `ProjectView` (brief, plan, milestones/phases, tasks with
  status/owner, roles tree controller→leads→workers, notices, autopilot state,
  workstreams).
- `project.init {root, brief?}` → creates `.atrium/` like the app does.
- `project.task.create|update|move {root, …}`; `project.notice.add {root, text}`;
  `project.autopilot.set {root, on}`.
- `project.kickoff {root, role, task?}` → the prompt text the app would send for
  that role (reuse the app's kickoff builders in atrium-project).
- `project.next {root, state}` → scheduler decision: which tasks are ready, which
  role/member should take each (pool spread), what to send whom. The worker turns
  these into agent launches/messages.
Team
- `team.read {root}` → members, roles, stand-ins, watch rules (from `.atrium/team.toml`).
- `team.write {root, team}`; `team.pick {team, quotas, ready, now}` → `Pick`;
  `team.roster {…}` → text.
Workflow
- `workflow.templates {dirs}` → list (builtin + user + project), with parse errors.
- `workflow.template.parse {text}` / `workflow.template.render {template}` (TOML out)
  / `workflow.template.validate {text}` → errors with line numbers.
- `workflow.run.start {template, inputs, dir}` → creates the run dir and `run.json`.
- `workflow.run.load {dir}` → `RunView`.
- `workflow.run.next {dir}` → effects: `launch {step, item?, prompt, agent?}`,
  `check {kind, …}`, `gate {step, choices}`, `done`.
- `workflow.run.report {dir, step, item?, answer}` → apply an agent's report.
- `workflow.run.check_result {dir, step, result}`;
  `workflow.run.decide {dir, step, choice, send_back_to?, note?}` (gates);
  `workflow.run.pause|resume|cancel {dir}`.
- `workflow.experience {template}` → accumulated experience notes.

Unit-test the ops natively (`cargo test -p atrium-orca-core`) and add one
Node test that loads the built wasm and runs a template end to end with fake
agent answers.

## Worker — `plugins/orca-atrium/worker`

`activate(orca)` (see `plugin-authoring.md`, `plugin-host-api.md`):

1. **Core**: load wasm once; serialize calls.
2. **Agent bridge** (how agents talk back — Orca plugins can't give agents tools):
   - Start a unix socket server (named pipe on Windows) under the plugin's
     storage dir.
   - `agent.launch.before` hook: prepend `<plugin>/bin` to `PATH` and set
     `ATRIUM_SOCKET`, `ATRIUM_RUN`/`ATRIUM_PROJECT`, `ATRIUM_ROLE` in `env` when the
     launch is one the worker started (match by a pending-launch table keyed
     by workspace + prompt marker).
   - `bin/atrium` (a tiny Node script, plus `atrium.cmd`) sends
     `{cmd, args, terminal: $ORCA_TERMINAL_HANDLE}` to the socket and prints the
     reply. Commands: `atrium report <text|-f file>`, `atrium status`,
     `atrium task done|blocked <id> [note]`, `atrium notice <text>`,
     `atrium ask <question>` (raises a "needs you" item), `atrium help`.
   - Prompts the worker sends tell the agent exactly which `atrium` command to run
     when it finishes (Chinese or English per setting).
3. **Turn tracking**: `agent.status.changed` → map `terminalId` to our
   step/role; `done`/`waiting` without a report → after a grace period nudge once
   via `terminal.sendText`, then mark the step `stalled` (needs you).
   `blocked` → needs you. Track quota-limit hits from status/terminal text and
   hand the work to a stand-in (`team.pick`).
4. **Run driver**: loop `workflow.run.next` → do effects:
   - `launch`: `workspace.create` (worktree, when the template/step asks) or reuse
     the run's workspace, then `agent.launch` with prompt; respect max parallel.
   - `check`: run commands with `child_process` (timeouts, cwd), PDF pages, then
     `workflow.run.check_result`.
   - `gate`: create a needs-you item; notify; wait for a UI decision.
   Persist everything through the core (run.json), so a worker restart resumes.
5. **Project scheduler**: per open project with autopilot on, loop
   `project.next` → launch roles (controller, leads, workers) as agents in their
   folders/worktrees; deliver notices; react to `atrium task done`.
6. **Needs-you inbox**: gates, `atrium ask`, stalled/blocked agents, briefs to
   confirm. Feeds the status bar item and the UI.
7. **UI service**: `orca.panels.handle(name, handler)` for every request in
   `protocol.ts`; `orca.panels.post(name, payload)` for live updates.
8. Storage: `storage:own` for small state (known projects, needs-you, settings
   cache); run and project state live in their own files.

Write the worker against a small `Host` interface so tests run it against a fake
host (no Orca needed): `test/` drives a whole workflow run and a project round
with scripted agent status events and `atrium report` calls.

## Protocol — `worker/src/protocol.ts` (excerpt; the file is the source of truth)

Requests (UI → worker, `window.orca.host.call('plugin.request', {name, params})`
per `plugin-host-api.md`):
- `overview.get` → `{ projects: ProjectSummary[], runs: RunSummary[], needs: NeedItem[], agents: LiveAgent[] }`
- `project.get {root}` → `ProjectView`; `project.init {root}`; `project.autopilot {root, on}`
- `project.task.create|update|move {...}`; `project.notice {root, text}`
- `team.get {root}` / `team.save {root, team}`
- `role.start {root, role}` / `role.stop {root, role}` / `agent.focus {terminalId}`
- `templates.list` → `TemplateSummary[]`; `template.validate {text}`
- `run.start {templateId, inputs, workspaceId?}` → `{dir}`; `run.get {dir}` → `RunView`
- `run.decide {dir, step, choice, sendBackTo?, note?}`; `run.pause|resume|cancel {dir}`
- `run.retry {dir, step, item?}`; `need.resolve {id, answer?}`
- `board.open {root}` / `run.open {dir}` (worker writes the link file and calls `editor.open`)
Posts (worker → UI): `overview.changed`, `project.changed {root}`, `run.changed {dir}`,
`needs.changed`, `agent.changed {terminalId, state}`.

## UI — the visual interface (the part the user will judge)

All frames are `ui: "html"` so we control the visuals fully. Preact + our own
SVG; no heavy chart libs. Colors come only from Orca's frame CSS variables
(`plugin-theme-tokens.md`, `--orca-*`) so light/dark and every Orca theme work.
Chinese and English strings from one table (`ui/src/i18n.ts`), following
`orca.locale` unless the setting overrides it. Typography: system UI font stack,
tabular numerals for counters; 4/8 px spacing grid; 6–10 px radii; subtle
motion (150–200 ms) for state changes; respect `prefers-reduced-motion`.

1. **Sidebar panel "Atrium"** (narrow, ~320–420 px):
   - Header: "需要你" count chip, running count, settings gear.
   - **Needs you** list first (gates with Approve / Send back ▾, questions with a
     reply box, stalled agents with Nudge / Open). This is the main daily surface.
   - **Projects**: card per project — name, milestone progress bar, roles as a
     row of avatar dots coloured by live state (working/waiting/blocked/idle),
     autopilot toggle, "Open board".
   - **Runs**: card per running workflow — template name, step progress
     (segmented bar), current step, elapsed time, "Open".
   - Empty states with one clear action ("Make this folder an Atrium project",
     "Start a workflow").
2. **Project command center** (`board` editor, full width):
   - Top bar: project name, brief (expandable), autopilot switch, add notice.
   - **Org chart** (SVG): controller at top, leads, workers beneath; each node
     shows agent kind icon, model, live state ring, current task, quota bar,
     stand-in badge; click → details drawer (focus terminal, stop, reassign).
     Animated edges when work is handed down.
   - **Task board**: Kanban by status (todo / ready / in progress / review /
     blocked / done) with milestone swimlanes toggle; drag to move (calls
     `project.task.move`); card shows owner avatar and task file link.
   - **Milestones timeline**: phases as a horizontal track with progress.
   - **Team tab**: roster editor (members, agent, model, effort, folders, stand-in
     chain drag-to-order, quotas), validation inline.
   - **Activity feed**: notices and handoffs, newest first.
3. **Run view** (`run` editor, full width):
   - **Step graph** (SVG DAG, left → right): node per step with status colour,
     attempt count, per-item mini-grid for `each` steps, check badges
     (pass/fail), gate nodes as diamonds. Send-back edges drawn as dashed curves
     back to their target. Current step pulses.
   - Right drawer for the selected step: prompt, latest report (rendered
     markdown), checks with logs, attempts history, experience notes, actions
     (Retry, Open agent, Approve / Send back to … with note).
   - Header: inputs summary, state, Pause/Resume/Cancel, elapsed.
4. **Template editor** (`template` editor): split view — form/graph on the left
   (steps as draggable nodes, connect `needs`, edit step fields, checks, gates,
   `each`), TOML on the right in sync; validation errors inline with line numbers
   (`template.validate`). Saving edits the document (custom editor edit API).

Mock host: `ui/src/mock/` implements `window.orca` (host.call, on, theme vars,
locale) with fixtures (a project with 1 controller, 2 leads, 4 workers, 12 tasks;
a CUMCM run mid-way with a gate waiting and one send-back). `ui/dev.html?view=…`
renders each view standalone for Playwright screenshots in light and dark
(Chromium is pre-installed: `require('playwright')`). Screenshots go to
`plugins/orca-atrium/docs/screenshots/`.

## Quality bar

- TypeScript strict; `tsc --noEmit` clean for worker and ui.
- `node --test` for worker engine tests; `cargo test -p atrium-orca-core`;
  `cargo test --workspace` still green; clippy clean on the new crate.
- No Orca-specific network, no telemetry, no secrets in files.
- Commit nothing yourself unless told; the integrator commits.
