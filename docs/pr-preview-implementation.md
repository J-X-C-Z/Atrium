# Building PR status, previews and agent environments

A brief for three backend pieces whose UI already exists. Each is small and
independent. Read `AGENTS.md` first.

## 1. Pull requests and CI on lead cards

**UI (done):** a lead's lane shows `Workstream.pr` as a badge: "#42 CI
passing / failing / running / merged". Clicking it opens the URL. The model
types are `PullRequest { number, url, state, checks }` and
`Checks { None, Pending, Passing, Failing }` in `atrium-project`.

**To build:**

- Read each lead with a branch (`Project::branch_of(id)` when worktrees are
  on, else none) with `gh pr view <branch> --json
  number,url,state,isDraft,statusCheckRollup,headRefOid,baseRefName,mergeable,mergeStateStatus,updatedAt,closedAt,mergedAt,author,comments,reviews`
  in the project root.
- Map the result:
  - `state`: `draft` if `isDraft`, else `open`, `merged` or `closed`, lowercased.
  - `checks`: `Failing` if any check concluded `FAILURE`, `CANCELLED` or
    `TIMED_OUT`; else `Pending` if any is still running; else `Passing` if
    there are any; else `None`.
- Store the result on the stream and save, but only when it changed. With
  no PR, set `pr = None` and check again every two minutes. When `gh` is
  missing or not logged in, log it once and retry on the slow cadence
  (15 minutes) instead of stopping forever.
- **Cadence and backoff (item 31, ported from T3's `PullRequestSyncReactor`):**
  one sweep a minute; an open PR read every sweep, a quiet one (no host
  update for 30 minutes) every 5 minutes, a recently closed one every 15
  minutes, and a merged or day-old-closed one not at all. A failed read
  backs off 1, 2, 4 … minutes up to 15. See `atrium_project::pr_watch`.
- **Watch (item 31, ported from T3's `evaluatePullRequestWatch`):** the
  first good read baselines what is already there (failures, conflicts,
  passed checks); afterwards a new failing check, all required checks
  passing, checks awaiting approval, a missing check report, a merge
  conflict, a required rebase, or a new/edited comment or review is news.
  Deduplicated by head SHA, check names and the comment watermark, so each
  failing commit wakes once. A comment-only loop stops after 10 wakes.
- **Auto-fix (autopilot on only):** news goes through the autopilot brief's
  `queue_autopilot_wake`/`wake()` path and its debounce, naming the failed
  checks and their run URLs; the lead can fetch logs itself with
  `gh run view <id> --log-failed`. A merge or close wakes the lead once and
  the poll stops.
- Tests: the JSON-to-`PullRequest` mapping from fixtures of `gh` output; the
  ported T3 watch cases; the cadence/backoff decisions; and end-to-end polls
  with a fake `gh`.

## 2. Previews of UI work

**UI (done):** a lead's lane shows `.atrium/previews/<id>.png`
(`Project::preview_of`) with a click to open it full size. Leads with a UI
are told in their kickoff to save one at the end of each phase. That covers
agents that can take screenshots themselves.

**To build** (for agents that cannot):

- `.atrium/launch.json`, as in Claude's desktop app:
  `[{ "name": "web", "command": "npm run dev", "port": 5173, "url": "/", "cwd": "web" }]`.
- When a lead finishes a phase (its "Phase … done" notice), check whether
  the preview is older than the phase. If so, run the first matching entry's
  command in `project.work_dir(lead)` (or its `cwd` inside it), wait for the
  port, and screenshot `http://127.0.0.1:<port><url>`. Use headless Chromium
  (`chromium --headless --screenshot=<path> --window-size=1280,800 <url>`)
  or Playwright. Save to `.atrium/previews/<id>.png`, then stop the server.
- Never more than one preview at a time per project, and time out after 2
  minutes.
- Tests: launch.json parsing; the "is the preview stale" decision.

Local previews start disabled. In the project menu, **Review local preview
commands…** shows the resolved project folder and its launch entries. Approval
is stored in this device's Atrium settings. If the configuration or resolved
project changes, disable the old approval and review the commands again.
Atrium uses the first entry whose working directory exists inside the lead’s
work directory. Use a foreground server command. Unix cleanup targets the
launch process group, and Windows cleanup targets the launch Job Object.
Removing the project, disabling previews, or quitting Atrium cancels the run.

## 3. Environments for ACP agents

**UI (done):** Settings → Environments stores named variable sets and the
active one (`Settings.environments`, `Settings.environment`).
`Workspace::session_env()` returns the active variables. Codex threads
already get them (`StartThread.env` → `shell_environment_policy.set`).

**To build:** ACP agents read their own process environment, so:

- Start ACP agent processes (`acp-client`'s process start, and the node's
  for remote agents if the protocol allows it) with `session_env()` added
  to their environment.
- When the active environment changes, show a notice: "Restart Claude Code
  (and the others) to use the new environment". Add a "Restart agents"
  action that reconnects each running ACP agent. Open sessions resume
  through `open_session_with`, as after a crash.
- Tests: the spawned command's environment includes the variables (inject
  the spawn function as the existing process tests do).
