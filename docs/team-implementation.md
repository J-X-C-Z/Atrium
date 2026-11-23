# Teams: wiring the team into projects

Read `AGENTS.md` first, then `crates/atrium-team`. It is small on purpose and
built and tested. This brief is about using it in the scheduler.

## Decisions (made with the user)

1. **The models decide, the app covers.** The user has Claude, ChatGPT and
   Gemini subscriptions and a DeepSeek API key. Strong models have small
   allowances. Cheap ones go in circles on open-ended work. The answer is
   **not** routing rules in code: models change faster than code, and rules
   in code decide things for a model that it would decide better. So:
   - the controller (and each lead) is told who is on the team, the user's
     notes on each, and who is spent right now. It picks an agent per task
     itself with `agent:`, as it already can;
   - the app does only what a model cannot see. A member whose usage limit
     is hit, or that cannot start, gets its stand-in. A session going in
     circles is reported to whoever gave it the task, who decides what to
     do.
2. **Keep it this small.** Do not add task kinds, tiers, routing tables,
   automatic hints, review grades or effort rules. If something seems
   missing, raise it in the PR instead of building it.
3. **Off by default.** With `team.on = false`, nothing changes: no roster,
   no stand-ins, no watch notices.

## What is built

- `atrium_team::Team`: members (agent, model, effort, `notes`, `stand_ins`)
  and the thresholds of the watch. `Team::load(Some(root),
  team_path().as_deref())` reads the project's `.atrium/team.toml`, then
  the app's `team.toml`, then falls back to the default.
- `pick(team, wanted, quotas, ready, now)`: who runs a task that asked for
  `wanted`, a member name (`gpt`) or a backend id (`codex`). It returns the
  member, or else its stand-ins in order, with `instead_of`. If nobody is
  available it returns `Wait { until }`. A `wanted` that is not on the team
  runs as asked.
- `roster(team, quotas, ready, now)`: the controller's "Your team" text.
- `Quotas::hit/cleared/spent_until`, `quota::is_limit_message`.
- `watch::Watch`: feed it `Signal`s; call `verdict(&team.watch)`; then
  `watch::notice(..)` gives the text for the parent and `reported(..)`
  resets the watch.
- In the app (`workspace_team.rs`):
  - `Workspace.team` holds the app's team and the quotas.
  - `save_team` and `reload_team` read and write the file.
  - `team_limit(backend, reached, resets_at)` marks every member on a
    backend. Codex's rate-limit updates already call it.
- Settings → 团队 lists the members. It shows the on switch, each member's
  notes (editable), stand-ins and state (ready, not available, limit
  reached until a time).

## Yours: one PR

1. **Per project.** Keep one `Team` per open project:
   `Team::load(Some(&project.root), team_path().as_deref())`. Reload it when
   `.atrium/team.toml` or the app's file changes. The quotas stay app-wide in
   `Workspace.team.quotas`, since a limit is per account.
2. **Tell the planners.** When the team is on, append `roster(..)` to the
   kickoff of the controller and of every lead. Do not append it to workers.
   When a member becomes spent or comes back, post the new roster as a
   notice from `atrium` to the controller and the leads. Wake them only
   under the existing `wake_allowed` rules. Add this one line to every
   kickoff when the team is on:
   `Lessons from the user's corrections go in <rules_file(backend)>; prefer
   turning each into a test or check.`
3. **Stand-ins on start.** In `tick_project`, where a task's backend is
   resolved (`task.agent ... unwrap_or(&owner.backend)`), run it through
   `pick` with `ready = backend_ready`.
   - Start the stream on `pick.agent`, with `pick.model` and `pick.effort`
     when given.
   - On `instead_of`, post one notice to the task's owner, for example:
     "`api` runs on gemini: claude's limit is reached until 15:40."
   - On `Wait { until }`, leave the task open and try again at `until`, or
     at the next tick when `until` is `None`. Do not fail the start.
   - A stream that is already running keeps its agent. Stand-ins apply only
     when a task starts.
4. **Limits from ACP agents.** When an ACP turn fails and
   `is_limit_message(error)` holds, call `team_limit(backend, true, None)`.
   Then restart the stream's task through `pick`, so its stand-in takes it.
   Do this through the autopilot's existing retry path, not a second one.
   Agents on other devices count under their full backend id
   (`claude@studio`).
5. **The watch.** Keep one `Watch` per running lead or worker stream.
   - Feed it from completed items: `Item::Command` becomes
     `Signal::Command`, and `Item::FileChange` becomes `Signal::Edit` (the
     paths and the joined diffs). A turn's end becomes `Signal::TurnEnded`
     with that turn's `last.total` tokens.
   - On a verdict, `Project::post` `watch::notice(..)` from `atrium` to the
     stream's parent, then call `reported`.
   - Do not interrupt the stream, and do not send it anything. The parent
     decides.
   - Drop the watch when the stream's task finishes.
6. **Show it** (the only UI you add):
   - a stream row that runs as a stand-in shows "替补 · gemini";
   - a watch notice shows in the atrium panel like any other notice.

   Add zh strings for both.

## Tests

- `atrium-project` or a new `scheduler` test, GPUI-free:
  - a task with `agent: claude` starts on gemini while claude is spent, and
    the owner gets one notice;
  - with everyone spent, the task stays open and is retried at `until`;
  - a running stream is not moved.
- With the team off, kickoffs are byte-for-byte what they were.
- End to end with the mocks: make `mock-acp --name claude` fail a turn with
  "usage limit". The task should move to the stand-in. The settings page
  should show the member as limit reached.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --all`.
