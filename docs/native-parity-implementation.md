# Native parity: what the official clients and T3 Code do, built into Atrium

Items 20–31 in `AGENTS.md`. The owner's goal: using Codex, Claude Code and
Antigravity through Atrium should feel like their own clients, the UI should
stay uncluttered, and devices should behave like one machine. T3 Code
(`pingdotgg/t3code`, MIT) and Agents-Anywhere (Apache-2.0) were studied as
references; take their ideas, not their code or text. Atrium stays Rust +
GPUI with its own `atrium-node`; nothing here depends on T3.

This is **track A**. Track B (`docs/t3-extension-plan.md`) tries running
agents through T3 Code instead, in parallel, done on the owner's Claude
side. Both sit under one agent-runtime seam (item 32), so the project,
team, memory and group-chat layers are written once; the owner keeps the
track that works better. Do not build track B.

## Principles for every item

- **Agents declare, the UI obeys.** A control is shown only when the shown
  chat's agent supports it (item 20). Never emulate a feature the agent does
  not have, and never show a control that silently does nothing.
- **Quiet by default.** Show a setting when it differs from the default or
  when the user needs to act; tuck the rest into one overflow menu.
- **Device-agnostic.** Everything here works the same for an agent on this
  computer and on another device through the node.
- **GPUI-free decisions.** Put rules (what to show, when to retry, how to
  merge) in functions that take `now` and plain data, with unit tests; the
  UI only renders them.
- **Mocks keep up.** Extend `mock-codex` / `mock-acp` for every protocol
  feature you use, and test end to end against them.

## 20. Agent capabilities (S–M)

One record per agent session saying what it can do, read by the UI instead
of checking agent names.

- Add `atrium_core::Capabilities` (images, steer, queue, interrupt, plan
  mode, approval modes offered, model choice, effort choice, service tiers,
  fork, rewind, compact, review, usage/limits, context window, slash
  commands, skills, MCP status, subagents). Fill it from Codex `initialize`
  + `model/list` (codex-client) and from ACP `initialize` capabilities,
  `_meta` and config options (acp-client `AgentInfo`), for local and remote
  agents alike.
- Composer, chat header and menus read it (`ui/composer.rs`, `ui/chat.rs`).
- Tests: capability mapping from recorded `initialize` payloads for Codex,
  Claude Code (ACP) and an ACP agent with nothing optional.

## 21. Composer: one model pill, quiet extras, shortcuts (S–M)

- Agent, model, effort and speed (Fast) collapse into one trigger reading
  like "GPT-6.2 Sol · High" (agent icon in front; device tag when remote).
  One popover with sections: agent (grouped by device), model, effort,
  speed. Today these are `agent_picker`, `model_picker`, `effort_picker`
  and the Fast toggle in `codex_tools`.
- The approval/access mode shows only when it differs from the user's
  default; otherwise it lives in an overflow "…" menu with the rest
  (plan mode, worktree, options from ACP `config_option`s that are not
  model/mode/effort).
- Shortcuts: open model picker, effort up/down, cycle approval mode, toggle
  plan mode (bind in `main.rs`, list them in the command palette).
- Tests: the pill label and which controls are visible, per capability
  set (pure function).

## 22. Queue and steer (M)

- While a turn runs, Enter sends according to a setting: steer into the
  running turn (when the agent can) or queue as the next turn; a modifier
  does the other. Agents without steering always queue.
- Queued messages show above the composer as editable, removable chips with
  "send now" (interrupts and sends). They survive an app restart (store per
  thread in the app's state) and are sent in order when the turn ends.
- Slash commands typed during a turn wait for the turn to end.
- ArrowUp in an empty composer recalls earlier prompts of this thread;
  a draft is kept per thread when switching chats.
- Interrupt is enabled only after the agent reported the turn started.
- Tests: queue state machine (enqueue, edit, reorder, send-now, turn end,
  restart) as a pure model; e2e with mock-codex and mock-acp.

## 23. Usage limits everywhere, and recovery (M)

Builds on the per-backend limits already in `workspace.rs` (`limits_for`,
`note_limits`) and the Claude usage reader (`agent_rpc::claude_usage`).

- A banner above the composer when the shown agent is near (≥90 %) or at
  its limit, with the reset time; a popover with every window (5-hour,
  weekly, per model) for every agent and device.
- Classify a failed turn as a usage-limit error (`atrium_team::quota::
  is_limit_message` plus structured codes where the agent gives them) with
  its reset time. Offer "continue when it resets": the app sends "continue"
  to that thread after the reset (persisted, survives restart). Projects
  under the autopilot do this automatically.
- Antigravity: find what usage it exposes (ACP `_meta`, its own files or
  CLI) and feed the same model; if nothing, say so in the popover.
- Context window: show used/total for every agent that reports it
  (Codex token usage, ACP `usage_update`); warn near the limit and offer
  compact where supported.
- Tests: parse and classification fixtures, reset scheduling (pure, `now`).

## 24. Settings that apply mid-thread (S–M)

- Codex: apply model, effort, tier and approval changes to a loaded thread
  with the app-server's settings update method instead of waiting for the
  next `turn/start`.
- ACP: `session/set_config_option` / `set_mode` immediately (already used;
  make sure the UI reflects the agent's `config_option_update` reply).
- When a change cannot apply mid-thread, say what happens ("from the next
  turn" / "starts a new session"), decided by one pure function.
- Tests: transition table per capability set.

## 25. Codex protocol completeness (M)

Compare against the current `codex-rs/app-server` README and implement the
user-visible gaps (see the audit in this item's PR description):

- MCP: server status list (a panel in Settings and a chip when a server is
  failing), OAuth login for MCP servers, and **elicitation requests**
  (today rejected; map onto the existing user-input UI).
- `skills/list` shown in the `/` and `$` menus; `config/read` for
  defaults.
- Thread delete, read state, `model/rerouted` notice, `command/exec` for
  `!command` in the composer.
- Read vs write: views and sync use `thread/read`; only sending in a
  thread resumes it, so Atrium does not take the writer lock from a Codex
  desktop app that has the thread open.
- Extend `mock-codex` for each, with e2e tests.

## 26. Claude Code natively (L)

ACP through the adapter loses Claude Code's own subagents, background
tasks, skills, usage and hooks. Add a direct backend that runs the
`claude` CLI in streaming JSON mode (input and output as JSON lines, with
partial messages and the permission-prompt control channel), as the
official Agent SDK does:

- New crate `claude-client`: spawn, handshake, send user turns (text and
  images), stream assistant/thinking/tool events, permission requests
  answered from Atrium's approval UI, interrupt, resume by session id,
  model and effort per turn, slash commands and skills list, usage and
  rate-limit events, subagent and background-task events shown as child
  items (`ui/steps.rs`, right panel subagents).
- Translate into `atrium_core::AgentEvent` like `codex-client::translate`.
- Works through the node like other agents (stdio is bridged already).
- Keep the ACP route as the fallback; the user picks per agent profile.
- `mock-claude` (scripted JSON lines) and e2e tests. Record real
  transcripts for fixtures on the owner's machine (no secrets).

## 27. Antigravity specifics (M)

Find what Antigravity offers beyond plain ACP (sign-in flow, usage, skills,
child work) and surface it through items 20 and 23. Document what you
found in this file.

## 28. Chat list by state (M)

- Sections: Pinned, Working (a turn or background task running, or
  waiting on the user — those first), Recent (by day), Archived. Waiting
  on the user shows a dot and sorts to the top.
- Every state change (archive, pin, delete) shows an Undo toast.
- Optional grouping by project/folder (a toggle).
- `ui/chat_list.rs`, `ui/sidebar.rs`. Pure tests for sectioning and order.

## 29. Changes panel and work log (M)

- A changed-files tree for the thread (and per turn) in the right panel,
  with a side-by-side or unified diff view; open in editor; revert file
  or hunk (reuse `ui/changes.rs` and the git panel).
- Tool steps: grouped rows with icon, short summary and a live timer;
  details open in an inspector instead of expanding inline
  (`ui/steps.rs`).
- Todo lists and plans as cards that update in place.

## 30. Rewind: edit from here (S–M)

Edit an earlier user message and continue from there: restore files with
`workspace_checkpoints.rs`, roll the agent back (Codex `thread/revert`;
ACP where supported, else start a new session with a summary), then send
the edited message. Hidden when the capability is missing.

## 31. Devices: live presence and threads that follow you (M–L)

- Presence from the live link: node heartbeat (the 15 s WebSocket ping),
  an immediate probe after the computer wakes from sleep, "reconnecting"
  while the link retries (state exists since a147ab8; finish the wake
  probe and heartbeat timeout).
- Per-device results for multi-device calls (`ok | offline | timeout |
  error`) so one slow device never blocks a list.
- Threads continue from any device: the node keeps an append-only event
  log per thread it runs, with a sequence number; any Atrium attaches with
  `after=<seq>` and gets a snapshot only when the log cannot cover the gap.
  Sending from a second device goes to the owning node, which writes to
  the agent (one writer). This replaces conversation replication for
  threads that run on a node. Update `docs/node-protocol.md`.
- Windows drive roots in folder browsing; preview files on a device with
  their media type.
- Tests: node log replay and gap → snapshot; two clients on one thread
  against mock-node.

## 32. Agent-runtime seam (M) — shared with track B

Move what the project layer needs from an agent behind one trait in a
GPUI-free crate (`atrium-runtime`): start a session (agent, model, effort,
folder, worktree, device, MCP servers, first message), send, steer,
interrupt, wait for the turn to end, read the transcript, list sessions,
limits. Implement it with today's clients (codex-client, acp-client, the
node). The scheduler in `workspace_project*.rs`, the workflow runner and
the team pick call the trait, not the clients. Tests: the scheduler's
existing e2e checks pass through the trait with mocks. Do this item first
or second; track B adds a T3 implementation of the same trait.

## Order

32 → 20 → 21 → 22 → 23 → 24 → 25 → 28 → 29 → 30 → 26 → 27 → 31. Items 20–24 are
what the owner sees every day; 26 is the largest and can run in parallel
after 20.

## Done means

Each PR: the item's tests, `cargo test --workspace`, clippy and fmt clean,
an Xvfb screenshot of any UI change in Chinese and English, zh strings for
every new `t()`, and the row in `AGENTS.md` ticked.
