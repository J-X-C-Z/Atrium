---
name: atrium
description: Work as the controller, a lead or a sub-agent of an Atrium project — a repository with an `.atrium/` folder where several agents work side by side. Use when your first message says you are a controller, lead or sub-agent of an Atrium project, when you see `.atrium/project.json`, or when asked to split work into leads, follow or tick a roadmap, hand work to a sub-agent, report a task done, or post a notice to other workstreams.
---

# Working in an Atrium project

Atrium runs a project as three levels of agents in one repository:

| Role | Does | Never |
| --- | --- | --- |
| **Controller** (主控) | Splits the project into 2–5 leads, writes each a phased roadmap, keeps `.atrium/plan.md`, re-aligns leads when woken | Writes code, takes over a lead's work |
| **Lead** (负责人) | Owns one part (its scope) for a long time, works through its roadmap phase by phase, hands self-contained pieces to sub-agents | Edits its own phases (asks the controller instead) |
| **Sub-agent** (子 agent) | Does one task for its lead inside the lead's scope, then reports | Starts agents, edits the brief, contracts, plan or other task files |

Nobody calls Atrium directly. Atrium watches the files in `.atrium/`, starts
the agent each task file asks for, and tells the asker when it finishes. You
are woken by notices and reports, so **never poll or wait in a loop**: finish
your turn and stop.

## Use the `atrium` tools when you have them

Atrium gives every project session an MCP server named `atrium`. It already
knows who you are; you cannot act as someone else.

| Tool | When |
| --- | --- |
| `whoami` | First, every session: your role, scope, goal, parent, roadmap with the current phase, and your tasks |
| `create_task` | Controller: once per lead, with `phases`. Lead: for a self-contained piece of work |
| `check_step` | Lead: the moment a step is done (`"2.1"`, or the step's text). Finishing a phase tells your parent |
| `finish_task` | When your whole task is done (`status: done`) or cannot be (`failed`), with a report |
| `notify` | Anything another workstream must know: a changed interface, a decision, a question across scopes. `to` is ids, `all` or `you` (the user) |
| `read_notices` | Rarely: notices already arrive with your next message |
| `write_status` | Lead: keep `.atrium/status/<you>.md` current (Focus, Done, Next, Touching, Questions) |
| `write_plan` | Controller: the plan |
| `list_tasks` | To see your tasks, or every task with `all` |

## Recipes

### Controller

1. `whoami`; read enough of the repository to plan.
2. `write_plan`: the goal, milestones in order, which lead owns each,
   decisions across leads. Put conventions in `.atrium/brief.md` and
   interfaces between leads in `.atrium/contracts/<name>.md`.
3. `create_task` once per lead:
   - `title`: the lead's name ("Web app").
   - `goal`: what done looks like and what not to touch.
   - `scope`: globs it owns (`web/**`); leads' scopes must not overlap.
   - `agent`: `codex`, `claude`, `deepseek` or `antigravity`, or leave out.
   - `phases`: 2–6, in order, **named by outcome** ("Timeline editing", not
     "Implement stuff"), each with a few steps small enough to check.
4. Stop. When woken by a lead's notice or report: check its work against the
   plan, update the plan, `notify` leads to re-align. Keep it short.

### Lead

1. `whoami`; read `.atrium/brief.md` and the contracts you depend on;
   `write_status`.
2. Work through the current phase. After **each** step, `check_step` — the
   user follows every lead on a flowchart built from these ticks.
3. Hand off work that is self-contained and checkable with `create_task`
   (`scope` inside yours; no `phases` needed). At most a few run at once;
   more wait. When told one finished, read its result (`list_tasks`) and
   review its changes before building on them.
4. Roadmap wrong? `notify` the controller; do not rewrite phases yourself.
5. When the whole goal is done: `finish_task` with `status: done`.

### Sub-agent

1. `whoami`; read `.atrium/brief.md`. Stay inside your scope.
2. Do the task. If something outside your scope must change, say so in your
   result instead of changing it.
3. `finish_task`: what you changed, how you checked it, what is left open.
   Then stop.

## Without the tools: the files

Everything the tools do is a plain file edit, so you can do it by hand.

```text
.atrium/project.json            Atrium's own bookkeeping. Never edit.
.atrium/brief.md                shared conventions and decisions
.atrium/contracts/*.md          interfaces between leads
.atrium/plan.md                 the controller's plan
.atrium/status/<id>.md          each workstream's status
.atrium/notices.md              append-only; one line per notice
.atrium/tasks/<owner>/<id>.md   a request for an agent one level down
```

A notice is one appended line:

```text
- 2026-11-19 14:05 · <your id> → <id, or all, or you>: <message>
```

A task file (the owner is the folder: the controller asks for leads, a lead
for sub-agents):

```markdown
# Web app
status: open
agent: claude
scope: web/**, src/ui

Build the web front end: routes, layout and the timeline editor. Do not
touch server/.

## Roadmap
### Skeleton
- [ ] Routes for every page
- [ ] Shared layout

### Timeline editing
- [ ] Drag clips on the timeline
- [ ] Undo and redo
```

- Tick a step: change its `- [ ]` to `- [x]`. When a phase is done, post the
  controller a notice.
- Finish: add `## Result` at the end with your report, then set the
  `status:` line to `done` (or `failed`, with the reason in the result).
- `status:` values are `open` (Atrium will start it), `running` (Atrium set
  it), `done` and `failed`. Do not set `running` yourself.
