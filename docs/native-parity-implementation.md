# Porting T3 Code's core to Rust (native parity)

Items 20–32 in `AGENTS.md`. Goal: Codex, Claude Code and Antigravity through
Atrium feel like their own clients, on this computer and on other devices.
T3 Code (`github.com/pingdotgg/t3code`, MIT) already solved most of this in
TypeScript. We port its **server core** to Rust, systematically, and verify
the port by running both on the same inputs. Atrium keeps its own UI, node,
projects, team, memory, workflows and Office work.

The earlier plan to run agents through T3 itself (`docs/t3-extension-plan.md`,
track B) is paused. `docs/t3-spike.md` stays useful: it records how to run
T3 headless against our mocks, which the differential tests below need.

## Licence and provenance

- T3 Code is MIT. Ported code is a derivative: put the MIT notice
  (`Copyright (c) 2026 T3 Tools Inc.` and the licence text) in
  `crates/atrium-engine/LICENSE-T3`, and a one-line header in each ported
  file naming the T3 source file it came from.
- `crates/atrium-engine/UPSTREAM.md` records the T3 commit or release the
  port is based on (start from release 0.1.11, the one `docs/t3-spike.md`
  ran) and, per ported module, its T3 source paths. Later syncs read the
  upstream diff since that commit and port it, then bump the record.

## What to port, what to skip

Port (server side): `packages/contracts` (the types and wire shapes),
`packages/provider-core` (adapter interface, capabilities),
`apps/server/src/orchestration-v2` (threads, turns, runs, queue, steering,
subagents, delegation, checkpoints, selection transitions, provider
switching, usage-limit recovery, thread streams), the Codex, Claude and
Antigravity adapters and the generic ACP adapter (`packages/provider-acp`,
`apps/server/src/provider/…`), usage readers
(`providerUsageLimits`, `claudeUsageLimits`), context budget, and the MCP
orchestration toolkits agents use (`apps/server/src/mcp/toolkits`).
Read `docs/internals/*` in their repo first, especially the providers
document's rules (declare only real capabilities; never emulate a
permission mode; hide plan mode when the provider has no plan cards).

Skip: the React web app, Electron, mobile apps, T3 Connect relay, pairing,
OAuth/DPoP and remote-access auth (Atrium uses Tailscale and `atrium-node`),
themes, MCP Apps rendering, the server's HTTP/WebSocket RPC surface (our UI
calls the engine in-process; the node exposes it to other devices, item 29).
Scheduled tasks, PR watch and T3 workflows overlap Atrium's own; item 31
decides case by case.

## Shape in Atrium

- `crates/atrium-engine` (no GPUI): contracts, event store, projections,
  orchestration, adapters, usage, MCP toolkit. Async, `now` injected,
  storage in SQLite (`rusqlite`, bundled) under the app's data folder.
- The app (`crates/atrium-app`) reads engine projections and calls engine
  commands; the project scheduler, workflow runner and team call it through
  the runtime seam (item 32).
- `codex-client` and `acp-client` become the transport layers under the
  Codex and ACP adapters (keep what works; port T3's adapter logic on top).

## Verification: differential tests

Tokens are not the bottleneck; proving the port right is. For every item:

1. **Port their tests.** Each ported module brings the T3 unit and
   integration tests that cover it, rewritten as Rust tests with the same
   cases and expected values.
2. **Run both on the same scenario.** `tools/t3-diff/` (a small Rust or
   Python harness) runs a scripted scenario twice: against the pinned T3
   binary (`t3 serve` with a scratch `T3CODE_HOME`, providers pointed at our
   mocks, driven by `t3 thread start/send/show --json`; see
   `docs/t3-spike.md`) and against `atrium-engine` with the same mocks. It
   dumps both normalized event streams (ids, timestamps and paths
   replaced by placeholders) and diffs them. Because the contracts are
   ported with T3's JSON names, the dumps compare field by field.
3. **Mocks both can drive.** `mock-codex` gets the fields T3 requires
   (listed in `docs/t3-spike.md`); add `mock-claude` (scripted streaming-JSON
   lines, the `claude` CLI's format) and use `mock-acp` for ACP agents.
   Scenarios: plain turn, tool call with approval, user-input request,
   plan, interrupt, steer, queue, usage-limit failure and recovery,
   subagent, compaction, rewind, resume after restart.
4. **Real agents last.** Each adapter item ends with a short script the
   owner runs on a real machine; record the transcripts (no secrets) as
   new fixtures.

CI runs the ported tests on all three systems; the differential suite runs
on Linux (it needs the T3 binary, cached by version).

## Items, in order

**32. Runtime seam (M).** One trait in a GPUI-free crate (`atrium-runtime`)
for what the project layer needs from an agent: start a session (agent,
model, effort, folder, worktree, device, MCP servers, first message), send,
steer, interrupt, wait for the turn, read, list, limits. Implement it with
today's clients; the scheduler (`workspace_project*.rs`), workflow runner
and team pick call it. Existing scheduler e2e checks pass through it.
The engine plugs in under this trait in item 22.

**20. Contracts (M).** Port `packages/contracts` that the core uses
(orchestration v2 events, items, commands, capabilities, model selection,
usage limits, provider instances) as serde types with identical JSON. Golden
tests: T3's fixtures and JSON from the spike round-trip unchanged. Set up
`LICENSE-T3`, `UPSTREAM.md` and `tools/t3-diff` here.

**21. Event store, projections, thread streams (M).** Append-only events
per thread with sequence numbers, projections the UI reads, stream replay
from `after=<seq>` with T3's caps and snapshot fallback. Restart recovery.

**22. Adapter interface and the Codex adapter (L).** Port the adapter
interface and capability record, then the Codex adapter over `codex-client`
(thread settings applied mid-thread, read-vs-write so a Codex desktop app
keeps its writer lock, MCP elicitation, skills, config). Wire the engine
under the runtime seam. First differential scenarios green.

**23. Claude adapter (L).** T3 uses the Claude Agent SDK, which drives the
`claude` CLI in streaming-JSON mode. Port the adapter and implement the
SDK's side of that protocol in a `claude-client` crate (spawn, user turns
with images, partial messages, permission requests on the control channel,
interrupt, resume, model and effort, skills, subagents and background tasks,
usage-limit events). ACP stays as a fallback per agent profile.
`mock-claude` plus differential scenarios; a real-machine script.

**24. ACP adapter and Antigravity (M).** Port the generic ACP adapter rules
(strict compliance, degrade through capabilities) and the Antigravity
flavor (sign-in, usage reader, skills, child work) onto `acp-client`.

Built: `providers/acp.rs` (capabilities, sessions, turns, steering,
interrupt, approvals and native questions, config options by category,
per-turn usage, commands, pump, contract attachment ids resolved through
the host's attachments directory, T3's model validation/custom
models/`alwaysSetModel`/unset defaults/context-window lookup, the
client-policy dispositions with per-session execute grants, a deferred
finalize and a prompt-inactivity watchdog), `providers/acp_antigravity.rs`
(sign-in methods and validation, authorization-URL capture, code login and
logout state machine, profile environment isolation, skills, typed local
usage reader, native prompt attachments, child-work launch parsing),
`acp_runtime.rs` (ACP under `AgentRuntime`; `limits()` reports only what
the agent streamed, `usage_scan` exposes the typed local records), and the
app's `ATRIUM_ACP_ENGINE` gate. Scenario 08 matches T3 0.1.11 over the same
ACP path; 09–13 are engine-only with reasons in the files. Gaps: forwarding
a pasted loopback redirect to the agent's listener and browser-suppression
verification (real-machine only), thread snapshots/fork/rollback, deep
subagent bookkeeping from local session databases, item 26's report (that
module is not in this slice), post-settle continuation runs (deferred
finalize is in), MCP-over-ACP and client terminals/fs, and the app settings
UI for the gate.

**25. Turn orchestration (M).** Queue vs steer per send (user setting, a
modifier for the other; queued runs stored and editable; slash commands
wait), interrupt gated on the turn-started signal, selection transitions
(apply next turn / restart session / hand off / reject), provider switch
with context handoff, subagent and delegation bookkeeping, the rule that
every terminal path settles open tools, subagents and requests.

**26. Usage, limits, context (M).** Usage readers per provider into one
report, usage-limit classification with reset time, recovery worker that
continues after the reset (persisted), context budget and idle
auto-compaction. Replaces the interim code in `workspace.rs`
(`limits_for`, `note_limits`) and `agent_rpc::claude_usage`.

**27. Checkpoints, rewind, fork (M).** Port checkpoint services and "edit
from here"; reuse Atrium's git checkpoint code where it already does the
same thing.

**28. MCP orchestration toolkit (M).** The tools agents call to launch,
message, wait on and read other threads and to delegate tasks. Merge with
Atrium's `atrium` MCP server (`crates/atrium-project/src/mcp.rs`): one
server, project task files stay the way projects divide work; delegation
is for ad-hoc help. Kickoffs say which to use when.

**29. Engine on the node (L).** `atrium-node` hosts an engine for the
agents it runs, so a thread lives on the device that runs it and any
Atrium attaches to it (stream from a sequence number; one writer). Replaces
pass-through stdio and conversation replication for those threads. Presence
from the live link with heartbeat and a probe after wake. Update
`docs/node-protocol.md`; mock-node tests with two clients on one thread.

**30. App on the engine (L).** The UI reads engine projections. Bring the
everyday experience in line, as T3 shows it: one model · effort · speed
pill with the agent icon and device tag; approval mode shown only when not
the default; an overflow menu for the rest; shortcuts for these; queued
messages as editable chips; prompt recall and per-thread drafts; usage
banner above the composer; chat list by state (Pinned, Working, Recent,
Archived) with undo; changed-files panel with diffs; grouped tool rows
with timers and an inspector; plan and todo cards; controls shown only
when the capability exists. Remove the app-side code the engine replaces.

**31. Overlaps (S–M each).** Compare T3's scheduled tasks, PR watch and
workflows with Atrium's scheduled chats, PR/CI polling and workflows; port
what Atrium lacks into Atrium's versions rather than keeping two.

## Done means

Per item: ported tests and the item's differential scenarios pass,
`cargo test --workspace`, clippy and fmt clean, `UPSTREAM.md` updated, an
Xvfb screenshot (Chinese and English) for UI changes, zh strings for every
new `t()`, and the row in `AGENTS.md` ticked.
