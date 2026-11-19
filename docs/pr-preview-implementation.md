# Building PR status, previews and agent environments

A brief for three backend pieces whose UI already exists. Each is small and
independent. Read `AGENTS.md` first.

## 1. Pull requests and CI on lead cards

**UI (done):** a lead's lane shows `Workstream.pr` as a badge: "#42 CI
passing / failing / running / merged". Clicking it opens the URL. The model
types are `PullRequest { number, url, state, checks }` and
`Checks { None, Pending, Passing, Failing }` in `atrium-project`.

**To build:**

- Every 2 minutes, for each lead with a branch (`Project::branch_of(id)`
  when worktrees are on, else none), run `gh pr view <branch> --json
  number,url,state,isDraft,statusCheckRollup` in the project root.
- Map the result:
  - `state`: `draft` if `isDraft`, else `open`, `merged` or `closed`, lowercased.
  - `checks`: `Failing` if any check concluded `FAILURE`, `CANCELLED` or
    `TIMED_OUT`; else `Pending` if any is still running; else `Passing` if
    there are any; else `None`.
- Store the result on the stream and save, but only when it changed. With
  no PR, set `pr = None`. When `gh` is missing or not logged in, log it once
  and stop polling.
- **Auto-fix (autopilot on only):** when checks turn `Failing`, wake the
  lead with the failing jobs' last 60 log lines (`gh run view <id>
  --log-failed`). Use the autopilot brief's `wake()` and its debounce, at
  most once per failing commit.
- Tests: the JSON-to-`PullRequest` mapping, from fixtures of `gh` output.

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
