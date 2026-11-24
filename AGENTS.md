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

**Next: port T3 Code's core to Rust, items 20–32** (brief in
[`docs/native-parity-implementation.md`](docs/native-parity-implementation.md)),
in the order the brief gives (32 first). Keep T3's MIT notice with ported
code. Running agents through T3 itself (`docs/t3-extension-plan.md`) is
paused; don't build it. Side lanes 33–37
([`docs/parallel-lanes.md`](docs/parallel-lanes.md)) don't touch the port: take
one whenever a port PR waits for review or a real-machine check. Earlier: items 14–17 are merged; real-file checks of Word,
PowerPoint and Excel are next. Real-device acceptance (multi-device items 12/13 and workflow W1 in
`docs/backend-acceptance-matrix.md`) is run by the owner on real machines;
do not mark it done from mocks.

| # | Task | Brief | Depends on |
| --- | --- | --- | --- |
| 1 | ✓ done in #1. **Project scheduler**: start leads from the controller's task files, report finished tasks, wake parents (debounced), tick every project, safety caps | [`docs/hierarchy-implementation.md`](docs/hierarchy-implementation.md) | — |
| 2 | ✓ done in #1. **Task pools**: start pool workers (`workers_needed`), claim-then-kickoff, keep idle workers claiming, wake leads with `pool_report`, retire and replace workers, release stale claims | [`docs/autopilot-implementation.md`](docs/autopilot-implementation.md) §1 | 1 |
| 3 | ✓ done in #1. **Autopilot** (unattended, 24 h): one `wake()` path, liveness continuation, heartbeats, stall watchdog, retry backoff, resume after restart, unattended approvals and questions, daily turn cap, keep the machine awake | [`docs/autopilot-implementation.md`](docs/autopilot-implementation.md) §2 | 1, 2 |
| 4 | ✓ done in #1. **atrium-node**: the per-device service that runs agents for other devices on the tailnet | [`docs/node-implementation.md`](docs/node-implementation.md), contract in [`docs/node-protocol.md`](docs/node-protocol.md) | — |
| 5 | ✓ done in #1. **Worktrees**: one git worktree and branch per lead and pool worker, worker tasks merged into the lead's branch, lead branches merged on request | [`docs/worktree-implementation.md`](docs/worktree-implementation.md) | 1, 2 |
| 7 | ✓ done in #1. **PR/CI status, previews, agent environments**: poll `gh` for lead branches (auto-fix on failing CI under the autopilot), screenshot UI work from `.atrium/launch.json`, start ACP agents with the chosen environment | [`docs/pr-preview-implementation.md`](docs/pr-preview-implementation.md) | 5 for PRs |
| 6 | ✓ done in #1. **Phone view API**: a tailnet-only HTTP API and event stream to follow projects, answer approvals and post notices from a phone (the web page is built on the UI side) | [`docs/remote-view-implementation.md`](docs/remote-view-implementation.md) | 1 |
| 8 | ✓ done in #6. **File previews** (UI and `atrium-preview` API built; fill in `Previewer::preview`/`find`): Word, PowerPoint, Excel, CSV and PDF in the side panel, read-only, in pure Rust (office2pdf → PDF, hayro → PNG, calamine for grids), Chinese fonts first, with a page cache and hard limits | [`docs/file-preview-implementation.md`](docs/file-preview-implementation.md) | — |
| 9 | ✓ done in #2. **Office core** (`atrium-office`; model, ops, change reports and the editor UI built): the file binding — lossless OOXML package and XML read into the model, and the recorded ops written back on save. Five PRs, in the order the brief lists | [`docs/office-editing-implementation.md`](docs/office-editing-implementation.md) | 8 for the page view |
| 10 | ✓ done in #3. **Multi-device sync**: every Atrium is a node; all devices' conversations in every sidebar; continue one remotely through its owner (one writer, followers); replicated overlay (HLC last-writer-wins, no secrets); offline cache; move a conversation with its code and session; one scheduler per project by lease. Six PRs, in the order the brief lists | [`docs/multi-device-sync-implementation.md`](docs/multi-device-sync-implementation.md) | 4 |
| 11 | ✓ done in #3. **Workflows** (`atrium-workflow`: template format, run state machine, file checks, prompts and the built-in math modeling template built; UI on the UI side): the runner that starts an agent per step, waits, checks (`command`, `pdf_pages`, `review`), retries with the reasons, sends faults back, stops at gates and survives restarts; template and run storage; the TeX environment check | [`docs/workflow-implementation.md`](docs/workflow-implementation.md) | — |
| 12 | ✓ done in #4. **Windows paths**: the git worktree, checkpoint and move tests fail on Windows with `os error 123`; make `Native backend acceptance` green on all three systems | [`docs/windows-git-paths.md`](docs/windows-git-paths.md) | 5 |
| 13 | ✓ done in #5. **Workflow runner, after a restart**: re-attach a step whose turn is still running instead of retrying it (the one runner limit left in the brief) | [`docs/workflow-implementation.md`](docs/workflow-implementation.md) "Yours" 2 | 11 |
| 14 | ✓ done (merged from Codex's branches). **Document looks in the file binding** (`look.rs` types and `WordOp::SetLook` built): read every font, size, spacing, indent, list, section, header/footer, table and picture value into the model, resolved through styles, numbering and theme; write formatting back as minimal `rPr`/`pPr`; the same for slides. Three PRs: 14a read Word, 14b write Word, 14c slides | [`docs/layout-engine-implementation.md`](docs/layout-engine-implementation.md) | 9 |
| 15 | ✓ done (merged from Codex's branches). **Layout engine** (`atrium-layout` API, navigation and a reference engine built; the page editor on the UI side): lay documents out as Word does — line breaking with Chinese rules, justification, tabs, line spacing and grid, keeps and widows, sections and columns, tables across pages, floating pictures, footnotes — and relayout fast enough for typing. Five PRs, 15a–15e | [`docs/layout-engine-implementation.md`](docs/layout-engine-implementation.md) | 14 |
| 16 | ✓ done (merged from Codex's branches). **Slides in full** (`ShapeLook` fill/stroke/path/shadow, `ShapeKind`, `Chart`, `Item::Path`/`Gradient` and their painting built): every shape in z-order with group transforms, preset and custom outlines, gradient and picture fills, lines with dashes and arrows, theme style references, backgrounds and master graphics; slide tables with table styles; charts drawn from cached values; SmartArt from its drawing part; rotation. Four PRs, 16a–16d | [`docs/slides-sheets-implementation.md`](docs/slides-sheets-implementation.md) | 14c, 15a |
| 17 | ✓ done (merged from Codex's branches). **Sheets in full** (`CellStyle`, `Workbook::styles`, `Cell::style`, `SheetLook`, conditional rules, `SheetOp::SetStyle`, `Workbook::shown` built; the grid on the UI side): read cell styles, number formats, column widths, row heights, hidden rows/columns, frozen panes, conditional formats and drawings; show values through their number formats and conditional rules; write formatting, sizes and panes back. Three PRs, 17a–17c | [`docs/slides-sheets-implementation.md`](docs/slides-sheets-implementation.md) | 16c for charts |
| 18 | ✓ done. **Teams** (`atrium-team`, the settings page and Codex's limit hook built): give the controller and leads the team roster, start tasks on a stand-in while a member's limit is hit or it cannot start, mark ACP agents' limits from failed turns, and tell a parent when its worker goes in circles. Small on purpose: no routing rules in code | [`docs/team-implementation.md`](docs/team-implementation.md) | 1, 3 |
| 19 | ✓ done. **Projects across computers** (`spread.rs`, the Atrium Workspace settings, remote start folders and the kickoff section built): the `atrium` tools over the network for agents on other computers (`/v1/mcp`), tool wording in their kickoffs, `device_list`/`device_read`/`device_copy`, notices when a computer writes a folder it does not own, the device on each lane | [`docs/spread-projects-implementation.md`](docs/spread-projects-implementation.md) | 4, 10 |
| 20 | ✓ done in #9. **Contracts**: T3's core types as serde types with identical JSON; `LICENSE-T3`, `UPSTREAM.md`, the differential test harness | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 32 |
| 21 | ✓ done in #10. **Event store and thread streams**: per-thread events with sequence numbers, projections, replay from a sequence, restart recovery | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 20 |
| 22 | **Adapter interface and Codex adapter**: capabilities, mid-thread settings, read-vs-write, elicitation, skills; engine under the runtime seam | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 21 |
| 23 | **Claude adapter**: `claude-client` over the CLI's streaming JSON (permissions, subagents, tasks, skills, usage), `mock-claude` | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 22 |
| 24 | **ACP adapter and Antigravity**: generic ACP rules, Antigravity sign-in, usage, skills, child work | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 22 |
| 25 | **Turn orchestration**: queue vs steer, interrupt gating, selection transitions, provider switch, subagent bookkeeping | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 22 |
| 26 | **Usage, limits, context**: usage readers, limit classification, continue after reset, context budget, auto-compaction | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 22 |
| 27 | **Checkpoints, rewind, fork** | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 22 |
| 28 | **MCP orchestration toolkit** merged with the `atrium` MCP server | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 25 |
| 29 | **Engine on the node**: threads live on the device that runs them and continue from any device; heartbeat and wake probe | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 21, 22 |
| 30 | **App on the engine**: model pill, quiet extras, queue chips, usage banner, chat list by state, changes panel, work log · 30b done in #33 | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 25, 26 |
| 31 | **Overlaps**: scheduled tasks, PR watch, workflows; fold what Atrium lacks into Atrium's own | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | 30 |
| 32 | ✓ done in #8. **Runtime seam** (`atrium-runtime`): the project layer talks to agents through one trait | [`docs/native-parity-implementation.md`](docs/native-parity-implementation.md) | — |
| 33 | ✓ done in #14. **Team gaps**: apply member model/effort, pool workers through `pick`, persist the stand-in badge, stand-in on ACP limits without the autopilot | [`docs/parallel-lanes.md`](docs/parallel-lanes.md) | — |
| 34 | **Soak and footprint**: hours-long headless runs with mocks, RSS/files/CPU over time, leak fixes, the same numbers for T3 | [`docs/parallel-lanes.md`](docs/parallel-lanes.md) | — |
| 35 | **Office vs LibreOffice**: render a generated corpus both ways, per-page diff report, fix the worst differences | [`docs/parallel-lanes.md`](docs/parallel-lanes.md) | — |
| 36 | ✓ done in #15. **Installers and updates**: dmg (signed when secrets exist), installer exe, AppImage/deb, node as a service, release workflow | [`docs/parallel-lanes.md`](docs/parallel-lanes.md) | — |
| 37 | ✓ done in #11. **Small cleanups**: the `&PathBuf` warning, symlinked-path keys, CI on pushes to main | [`docs/parallel-lanes.md`](docs/parallel-lanes.md) | — |

Do one item per pull request. In each PR, tick its row here (add "✓ done in
#N") and list which of the brief's tests and end-to-end checks you ran.

## Where things are

```
crates/atrium-core      backend-agnostic model: threads, items, AgentEvent, Transcript
crates/agent-rpc        stdio JSON-RPC transport; node client; mock-node
crates/codex-client     Codex app-server client; mock-codex
crates/acp-client       ACP client; mock-acp (--name claude | deepseek | antigravity)
crates/atrium-project   projects, no GPUI: files, roles, tasks and roadmaps (lib.rs),
                        projects across computers (spread.rs),
                        agent operations (agent.rs), task pools (pool.rs),
                        autopilot model (autopilot.rs), the `atrium` MCP server (mcp.rs, device tools in devices.rs),
                        the agent skill (skill.rs, text in skills/atrium/SKILL.md)
crates/atrium-office    Word/Excel/PowerPoint model, looks (look.rs) and the lossless file binding
crates/atrium-layout    pages from the model: lines, items, caret and hit testing (no GPUI)
crates/atrium-team      the team: members and notes, stand-ins, limits, the roster, the watch
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
