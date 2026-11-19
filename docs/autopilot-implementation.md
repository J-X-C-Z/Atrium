# Building the task pool and the autopilot

A brief for the scheduler half of two features. The data model, the agent
tools and the UI exist and are tested; this file says what the scheduler must
do with them. Read `docs/hierarchy-implementation.md` first: this builds on
its `tick_project`, `start_stream` and wake logic.

The design follows [Paperclip](https://github.com/paperclipai/paperclip)'s
control plane, adapted to Atrium's files:

| Paperclip | Atrium |
| --- | --- |
| Issue checkout: one owner, atomic, 409 on conflict, idempotent for the holder | `Project::claim_next` under `TaskLock` (`.atrium/tasks/.lock`); a holder gets its task back |
| Heartbeats: short runs woken by timers and events, same session resumed | `Wake::Heartbeat` / `Wake::TurnEnded` continuations into the same thread |
| Liveness recovery: in-progress work with no live run gets a continuation | `Project::has_work` + `Project::continuation(stream, Wake)` |
| Silent active-run watchdog | stall detection (§2.3) |
| Orphan-run recovery after restart | `Wake::Restarted` (§2.5) |
| Task watchdog on a stopped subtree | the controller heartbeat (§2.2) |
| Budgets and pause holds | `autopilot.daily_turns`, user pause (§2.7) |
| Fresh session when context drifts | pool workers replaced after `tasks_per_session` (§1.4) |

## 1. The task pool

**The model.** A lead no longer gets one agent per task. Its tasks
(`.atrium/tasks/<lead>/*.md`) form a **pool**. A few long-lived **pool
workers** (role `worker`, `task: None`, `parent: <lead>`) each claim a task,
finish it, and claim the next, without the lead. The lead is woken only when
the pool runs low, with a digest of the results.

- `Project::startable()` now returns **only the controller's** tasks (leads to
  start). Leads' tasks never get a `stream_from_task`.
- Task files gained `claimed_by:`, `after:` (dependency ids) and `priority:`.
- `finish_task` (MCP) for a pool worker writes the result and **claims the
  next task in the same call**, returning it. A worker with the tools runs the
  whole pool in one turn chain without Atrium doing anything.

| Need | Call (`atrium-project`) |
| --- | --- |
| How many pool workers to start for a lead now | `Project::workers_needed(lead)` |
| A new pool worker `Workstream` | `Project::pool_worker(lead, backend)` |
| Its kickoff (includes the claimed task if any) | `Project::kickoff(&stream)` |
| Claim for a worker (atomic) | `Project::claim_next(worker)` |
| Put a task back | `Project::release(key)` |
| Pool counts and who holds what | `Project::pool(lead)` |
| The lead's wake text, or `None` | `Project::pool_report(lead)` → `PoolReport { text, keys }` |
| Mark it delivered | push `keys` into `project.file.reported_tasks`, `save()` |

### 1.1 Starting pool workers (every tick, every project)

For each lead with `workers_needed(lead) = n > 0`:

1. backend: the lead's (or a task's `agent:` when every ready task names the
   same one; otherwise the lead's).
2. Create `n` workers with `pool_worker`, push, save.
3. **Claim first, then kick off**: `claim_next(worker.id)`, then
   `start_stream(root, worker)`. The kickoff then contains the task, so an
   agent without the MCP tools can still do it.

### 1.2 Keeping workers busy

A pool worker whose turn ended idle:

- holds a running task (`task_of` is `Some`): it stopped mid-task → continue
  it (§2.1).
- holds none and `claimable(lead)` is not empty: `claim_next`, then send it
  `continuation(worker, Wake::TurnEnded)` (which names the task). This covers
  agents without the tools.
- holds none and nothing is claimable: leave it idle. If the pool stays empty
  for 10 minutes and the lead's task is done or the lead is idle, retire it
  (§1.4) to free the slot.

### 1.3 Waking the lead to refill

Every tick, for each lead: `if let Some(report) = pool_report(lead)`, wake the
lead (§2 wake rules, debounce included) with `report.text`, then mark
`report.keys` reported. `pool_report` already decides *when*: fewer ready
tasks than workers (or `autopilot.refill_at`) while results wait, or any
failure. It returns `None` while the pool is busy and healthy, so calling it
every tick is fine.

### 1.4 Retiring and replacing workers

A pool worker is replaced by a fresh session after
`autopilot.tasks_per_session` finished tasks (count tasks with its
`claimed_by:` and a finished status), between tasks only: set `retired =
true`, save, Codex `goal_set(..., Complete)`; the next tick's
`workers_needed` starts a new one. Also retire a worker whose turn failed 3
times in a row on the same task, after `release`-ing that task.

### 1.5 Stale claims

A `running` task whose `claimed_by` worker is retired, removed, or has had no
live turn for `autopilot.stall_min` minutes while idle → `release(key)`.

## 2. The autopilot: running around the clock

`project.file.autopilot` (`Autopilot`, saved in `project.json`; toggled from
the project menu, "Run unattended, around the clock"). Everything below runs
only while `autopilot.on`; with it off, today's behaviour stays (agents stop
when their turn ends and wait for the user).

Every wake goes through one function, `wake(root, stream_id, Wake, extra)`,
which:

- does nothing if the stream's thread is waiting on the user (approval or
  question; see §2.6 for how unattended mode avoids that), or retired;
- debounces: at most one wake per stream per 30 s; text arriving in between
  is merged into the next one;
- counts toward `daily_turns` (§2.7);
- sends `project.continuation(&stream, why)`, plus `extra` (a pool report,
  notices), into the stream's thread, through `with_stream_notices` so unread
  notices ride along. Codex: also re-`goal_set(Active)` if its goal stopped.

### 2.1 Liveness: a turn ended, work remains

When a project thread's turn completes (`TurnCompleted`, status completed)
and `has_work(stream)` is true, schedule `wake(.., Wake::TurnEnded)` after
`autopilot.continue_after_secs`. Cancel it if anything else starts a turn
first. Leads mid-pool with nothing to do themselves still have `has_work`
(their task is open); give them the heartbeat cadence instead of 60 s when
their own roadmap step is waiting on the pool (current phase unchanged and
`pool(lead).running` non-empty).

### 2.2 Heartbeats

- an idle lead with `has_work`: `Wake::Heartbeat` every
  `lead_heartbeat_min` minutes since its last turn ended;
- the controller with `has_work`: every `controller_heartbeat_min` minutes.
  This is the task-watchdog pass: it sees every lead's phase and re-aligns.

Keep timers in memory; recompute from "last turn ended" after a restart.

### 2.3 Stalls

A running turn with no `AgentEvent` for `stall_min` minutes: interrupt it
(`turn_interrupt` / ACP `cancel`), then `wake(.., Wake::Stalled)`. Three
stalls in a row on the same task: post the parent a notice and, for a pool
worker, release the task and retire the worker.

### 2.4 Failures

A turn that fails (error event, rate limit, overload, backend disconnect):
retry with `Wake::Retry(n)` after 1, 2, 4, 8, 16, then every 30 minutes,
forever while the autopilot is on (the failure may be a usage limit that
resets in hours). A backend process that died: restart it with today's
reconnect logic first, then resume the thread (`thread_resume_with` /
`open_session_with`, passing `mcp_servers` as `mcp_for_thread` gives).

### 2.5 Restarts

On launch, for every project with the autopilot on: for each stream with
`has_work`, resume its thread and `wake(.., Wake::Restarted)` (staggered, 5 s
apart). `running` tasks whose worker is gone are released (§1.5).

### 2.6 Never waiting on the user

In unattended mode the kickoffs tell agents not to ask (already done in
`Project::kickoff`). The scheduler covers what still reaches the user:

- **Approvals** from project threads: accept when the command or file is
  inside the project root (Codex: start project threads with sandbox
  `workspace-write` and approval `on-request`; answer `AcceptForSession` for
  in-root requests), decline otherwise and post the stream's parent a notice
  naming what was declined. Log every automatic answer as a notice from
  `atrium` to `you`, so the user can audit them.
- **Questions** (`UserInputRequest`): answer with "No one is available to
  answer; choose the reasonable option, note it in your status file and tell
  your parent." and post the question to the parent as a notice.

### 2.7 Limits and the off switch

- `daily_turns` (0 = none): turns started by the scheduler per project per
  local day. Over it: stop waking, post one notice to `you`, resume at
  midnight.
- The project-wide cap of 12 running streams from the hierarchy brief
  stays; pool workers count.
- Turning the autopilot off cancels every pending wake and timer at once.
  Running turns finish normally.

### 2.8 Keeping the machine awake

While any project has the autopilot on and a stream with `has_work`:

- macOS: hold an `IOPMAssertionCreateWithName(kIOPMAssertPreventUserIdleSystemSleep)`
  (or spawn `caffeinate -i -w <atrium pid>`); release it when nothing
  qualifies.
- Linux: `systemd-inhibit --what=idle:sleep` the same way.

Agents on another device (`agent@device`) keep running on that device, which
must stay awake itself; that is the way to run truly 24 h (a server running
`atrium-node`).

## 3. Tests

Pure decisions first (`atrium-project` has the model ones; put the timer
logic in a GPUI-free `scheduler.rs` taking `now` as an argument):

1. `workers_needed` starts at most `max_workers`, counting idle workers.
2. Claim-then-kickoff: the kickoff of a new pool worker contains its task.
3. A lead with a 6-task pool and 2 workers is woken exactly once when 1
   ready task is left, with both workers' results; not again until new
   results.
4. A failed pool task wakes the lead at once.
5. Liveness: a lead whose turn ends with an open task gets a `TurnEnded`
   wake after `continue_after_secs`; one with a done task does not; nothing
   is sent with the autopilot off.
6. Heartbeat and stall timers fire at the right `now`, and a new turn
   cancels a pending continuation.
7. Retry backoff: 1, 2, 4, 8, 16, 30, 30… minutes.
8. Restart: streams with `has_work` are woken with `Restarted`; stale claims
   are released.
9. Unattended approvals: in-root accepted, outside declined with a notice.

End to end on Xvfb with the mocks: a lead with `create_tasks` of 6 tasks and
`max_workers = 2`; see two workers start, each finish and claim on its own
(mock-acp's turn ends → §1.2 claims and continues it), the lead woken once
with the digest; with the autopilot on, kill and relaunch Atrium mid-pool and
see the workers resume.

## Done when

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` are clean.
- The scenario above runs to an empty pool with no user input, with a Codex
  and with a Claude (ACP) lead.
- A project left unattended overnight with the mocks (or a shortened clock in
  the scheduler tests) keeps every stream with work moving.
