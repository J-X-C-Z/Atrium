# Atrium: notes for coding agents

Atrium is a Rust + GPUI desktop app: one place to run Codex (app-server) and
ACP agents (Claude Code, DeepSeek, Antigravity), as chats or as projects
where a controller, leads and sub-agents work side by side. The README (in
Chinese) describes the product; this file is the working brief.

## Division of labour

The UI, the data model (`crates/atrium-project`) and the file formats are
done and tested. **The backend is yours**: the scheduler that turns the
model into running agents, and `atrium-node`. Work through the backlog below
in order. Each item has a brief in `docs/` that says what to build, which
existing functions to call, the tests to write and when it is done.

## Backlog

**Next up, in this order: 12, 8, 13.** Real-device acceptance (multi-device
items 12/13 and workflow W1 in `docs/backend-acceptance-matrix.md`) is run
by the owner on real machines; do not mark it done from mocks.

| # | Task | Brief | Depends on |
| --- | --- | --- | --- |
| 1 | ✓ done in #1. **Project scheduler**: start leads from the controller's task files, report finished tasks, wake parents (debounced), tick every project, safety caps | [`docs/hierarchy-implementation.md`](docs/hierarchy-implementation.md) | — |
| 2 | ✓ done in #1. **Task pools**: start pool workers (`workers_needed`), claim-then-kickoff, keep idle workers claiming, wake leads with `pool_report`, retire and replace workers, release stale claims | [`docs/autopilot-implementation.md`](docs/autopilot-implementation.md) §1 | 1 |
| 3 | ✓ done in #1. **Autopilot** (unattended, 24 h): one `wake()` path, liveness continuation, heartbeats, stall watchdog, retry backoff, resume after restart, unattended approvals and questions, daily turn cap, keep the machine awake | [`docs/autopilot-implementation.md`](docs/autopilot-implementation.md) §2 | 1, 2 |
| 4 | ✓ done in #1. **atrium-node**: the per-device service that runs agents for other devices on the tailnet | [`docs/node-implementation.md`](docs/node-implementation.md), contract in [`docs/node-protocol.md`](docs/node-protocol.md) | — |
| 5 | ✓ done in #1. **Worktrees**: one git worktree and branch per lead and pool worker, worker tasks merged into the lead's branch, lead branches merged on request | [`docs/worktree-implementation.md`](docs/worktree-implementation.md) | 1, 2 |
| 7 | ✓ done in #1. **PR/CI status, previews, agent environments**: poll `gh` for lead branches (auto-fix on failing CI under the autopilot), screenshot UI work from `.atrium/launch.json`, start ACP agents with the chosen environment | [`docs/pr-preview-implementation.md`](docs/pr-preview-implementation.md) | 5 for PRs |
| 6 | ✓ done in #1. **Phone view API**: a tailnet-only HTTP API and event stream to follow projects, answer approvals and post notices from a phone (the web page is built on the UI side) | [`docs/remote-view-implementation.md`](docs/remote-view-implementation.md) | 1 |
| 8 | **File previews** (UI and `atrium-preview` API built; fill in `Previewer::preview`/`find`): Word, PowerPoint, Excel, CSV and PDF in the side panel, read-only, in pure Rust (office2pdf → PDF, hayro → PNG, calamine for grids), Chinese fonts first, with a page cache and hard limits | [`docs/file-preview-implementation.md`](docs/file-preview-implementation.md) | — |
| 9 | ✓ done in #2. **Office core** (`atrium-office`; model, ops, change reports and the editor UI built): the file binding — lossless OOXML package and XML read into the model, and the recorded ops written back on save. Five PRs, in the order the brief lists | [`docs/office-editing-implementation.md`](docs/office-editing-implementation.md) | 8 for the page view |
| 10 | ✓ done in #3. **Multi-device sync**: every Atrium is a node; all devices' conversations in every sidebar; continue one remotely through its owner (one writer, followers); replicated overlay (HLC last-writer-wins, no secrets); offline cache; move a conversation with its code and session; one scheduler per project by lease. Six PRs, in the order the brief lists | [`docs/multi-device-sync-implementation.md`](docs/multi-device-sync-implementation.md) | 4 |
| 11 | ✓ done in #3. **Workflows** (`atrium-workflow`: template format, run state machine, file checks, prompts and the built-in math modeling template built; UI on the UI side): the runner that starts an agent per step, waits, checks (`command`, `pdf_pages`, `review`), retries with the reasons, sends faults back, stops at gates and survives restarts; template and run storage; the TeX environment check | [`docs/workflow-implementation.md`](docs/workflow-implementation.md) | — |
| 12 | ✓ done in #4. **Windows paths**: the git worktree, checkpoint and move tests fail on Windows with `os error 123`; make `Native backend acceptance` green on all three systems | [`docs/windows-git-paths.md`](docs/windows-git-paths.md) | 5 |
| 13 | ✓ done in #5. **Workflow runner, after a restart**: re-attach a step whose turn is still running instead of retrying it (the one runner limit left in the brief) | [`docs/workflow-implementation.md`](docs/workflow-implementation.md) "Yours" 2 | 11 |

Do one item per pull request. In each PR, tick its row here (add "✓ done in
#N") and list which of the brief's tests and end-to-end checks you ran.

## Where things are

```
crates/atrium-core      backend-agnostic model: threads, items, AgentEvent, Transcript
crates/agent-rpc        stdio JSON-RPC transport; node client; mock-node
crates/codex-client     Codex app-server client; mock-codex
crates/acp-client       ACP client; mock-acp (--name claude | deepseek | antigravity)
crates/atrium-project   projects, no GPUI: files, roles, tasks and roadmaps (lib.rs),
                        agent operations (agent.rs), task pools (pool.rs),
                        autopilot model (autopilot.rs), the `atrium` MCP server (mcp.rs),
                        the agent skill (skill.rs, text in skills/atrium/SKILL.md)
crates/atrium-app       the GPUI app; Workspace state and backend calls in
                        workspace*.rs (projects: workspace_project.rs); UI in ui/
```

The scheduler goes in `crates/atrium-app/src/workspace_project.rs`. Put its
decisions (what to start, whom to wake, when timers fire) in GPUI-free
functions that take `now` as an argument, in `atrium-project` or a new
`scheduler.rs`, and test those there.

## Rules

- Do not change the `.atrium/` file formats or the meaning of the existing
  `Project` functions; the UI and the MCP server read the same data. Add
  functions, with tests.
- Only the app writes `.atrium/project.json`. Agents and the MCP server
  write task files, `plan.md`, `status/` and `notices.md`. Rewrite task files
  under `pool::TaskLock`.
- Pass `Workspace::mcp_servers(root, &stream.id, &backend)` to every project
  session you start, and `mcp_for_thread` when you resume one.
- Do not edit `ui/` except to show a state you add. New user-facing strings
  go through `t` / `tf` with a Chinese entry in `crates/atrium-app/src/i18n.rs`
  (a test checks this).
- `third_party/ely-gpui-components` is a patched vendored copy; leave it alone.

## Checks

```bash
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

All three must be clean. For end-to-end checks, run the app against the mocks
(`CODEX_BIN=target/debug/mock-codex`, `mock-acp --name claude`) as the
README's testing section describes. On Linux use Xvfb.
