# Building multi-device sync

A brief for making Atrium the same on every computer you use. Whichever
machine you sit at, you see every conversation and project from all your
machines, and you can continue any of them. Read `AGENTS.md`,
`docs/node-protocol.md` and `docs/node-implementation.md` first. This builds
on `atrium-node`.

## Decisions (made with the user)

1. **Peer to peer over Tailscale only.** No server and no account; data
   never leaves your devices. Devices that are offline catch up when they
   come back.
2. **Opening a conversation from another computer continues it there by
   default** (remote attach). The agent keeps running on the computer that
   owns the conversation. **"Move to this computer"** is an explicit action
   that transfers the code changes and the session.

## What we take from Agents-Anywhere

[anywhere-labs/Agents-Anywhere](https://github.com/anywhere-labs/Agents-Anywhere)
(Apache-2.0) solves the same problem with a central server: PostgreSQL and
Redis, a Connector on each work device, and clients on desktop, phone and
web. We do not copy its architecture or code. We do adopt these lessons from
its docs (`docs/codex-ipc-takeover.md`, `docs/session-cache.md`,
`docs/claude-session-lifecycle.md`,
`docs/runtime-protocol/timeline-identity.md`):

- **Reading never takes ownership.** Use Codex's `thread/read` to show or
  sync a conversation, never `thread/resume`, which takes the writer lock
  (`~/.codex/thread-writer-locks/<id>.lock`). Taking it would make Codex
  Desktop or the TUI report "open in another app". Resume only to write
  (send a turn), and only on the device that owns the conversation.
- **One writer per conversation**; the others are followers. A follower
  sends its turn, steer, interrupt or approval to the owner, and the owner
  executes it.
- **Stable item identity.** The same tool call seen live and seen again
  when history is re-read must have the same id. That is the backend's
  native item id, plus the turn for Codex and the `tool_use_id` for Claude.
  Never deduplicate by text or hash.
- **Snapshot plus cursor.** Open a conversation with a snapshot, then apply
  events after a cursor. When you come back to it, catch up from the
  cursor, keeping the scroll position and expanded items. Rebuild from a
  snapshot when the owner says the cursor is too old.
- **Conversations started outside Atrium count too**, for example in the
  Codex TUI or Claude Code in a terminal. They appear on that device's
  list, read-only until you write to one.
- **Claude Code resumes from its native session id** after its process is
  gone. An idle agent process is closed after 10 minutes and resumed on the
  next turn.

## Concepts

- **Device.** One Atrium install:
  - `Settings::install_id` (already exists) is its id;
  - it has a name, from the Tailscale host name and editable;
  - and an OS, an app version and a sync protocol version.
- **My devices.** The devices whose Tailscale **owner** (`tailscale whois`
  user) is the same as this device's. These join automatically. Any other
  device joins only by pairing (§1.3).
- **Conversation ref.** `{ device, backend, thread }`. The device in it is
  the conversation's **owner**: the one whose agent holds the session.
- **Overlay.** Atrium's own data about conversations and projects:
  - titles you gave, pins, hidden, project membership;
  - the project list, environments, MCP servers and schedules.
  This is what gets replicated. Agent sessions are not copied, except by
  "Move to this computer".

## 1. Every Atrium is a node

### 1.1 Embedded node

- On start, Atrium runs the node in-process (`atrium-node` as a library,
  with a `serve` that takes a shutdown handle), on the same port and with
  the same security rules as today:
  - listen on the Tailscale address and loopback only;
  - accept non-loopback callers only after the Tailscale identity check;
  - fail closed when that check fails.
- If a standalone `atrium-node` service already listens on this machine,
  for example one installed with `atrium-node install`, use it instead and
  do not start a second one. `GET /v1/node` says which `install_id` it
  serves.
- **Roots** (what other devices may reach) default to the folders this
  Atrium has used: `recent_folders`, `projects` and the folders of local
  conversations.
- The first time sync is turned on, Atrium shows this list and asks before
  sharing anything. It can be edited later in Settings → Devices.
- New settings: `sync_enabled` (default on after that first consent) and
  `device_name`.

### 1.2 Device directory

- Add `GET /v1/atrium` to the node: `{ install_id, device_name, os,
  app_version, sync_version, owner }`. The node fills it from the embedded
  Atrium. A standalone node with no Atrium answers 404, and stays usable
  the old way, as `agent@device`.
- **Discovery** stays as today: Tailscale peers on port 7381, plus addresses
  added by hand. Keep a `devices.json` of known devices with their last
  contact time.
- Mismatched `sync_version`: show the device as "needs an update" and do
  not sync with it.

### 1.3 Pairing

A device that is not one of "my devices" joins by pairing:

1. One device shows a 6-digit code.
2. The other enters it.
3. Both store a long-lived token.

The token is sent as the node's existing bearer token. Pairing is never
needed between devices with the same Tailscale owner.

## 2. Replicated overlay: the `atrium-sync` crate

A new crate without GPUI, all pure functions plus a file-backed store, so
it can be fully unit-tested.

- **Model**:
  - a map from keys to **last-writer-wins registers**;
  - each write is stamped with a **hybrid logical clock** `(wall_ms,
    counter, install_id)`, compared in that order;
  - deletes are tombstones, purged after 30 days.
- **Keys**:
  - `thread/<device>/<backend>/<id>/{title,pinned,hidden,project}`;
  - `project/<project_id>` (§6);
  - `env/<name>`, `mcp/<name>` and `schedule/<id>`.
- **Storage**: an append-only `data_dir/Atrium/sync/ops.jsonl`, plus a
  compacted `state.json`, rewritten when the log passes 5 MB. Write with
  temp file, then fsync, then rename.
- **Exchange**, over the node:
  - `GET /v1/sync/vector` returns the version vector: the highest clock
    seen per device.
  - `POST /v1/sync/ops` with the ops the other device lacks. Pull and push
    both happen on connect, and then every 30 s while connected.
  - `GET /v1/sync/events` is a WebSocket that pushes new ops live.
- **Never replicated**:
  - **secrets**: the values of environment variables, MCP server `env`
    and headers, the node token, the phone push topic. Only their names
    are synced, and each device fills in its own values ("not set on this
    computer");
  - **per-device settings**: agent binary paths (`agent_commands`,
    `codex_binary`), `folder_modes`, `cwd`, the theme, sidebar state.
- **Schedules** carry a `device` field and run only on that device. Other
  devices show them as "runs on studio".

The Workspace reads its overlay from the sync store instead of
`Settings` for the replicated keys. On first start after upgrading, migrate
the existing values into ops stamped with this device.

## 3. All conversations on every device

### 3.1 Each device publishes its list

- `GET /v1/sync/threads?since=<ms>` returns this device's conversations as
  `ThreadMeta` plus `{ owner: install_id, state }`:
  - `state` is `idle`, `running`, `waiting_for_approval` or `waiting_for_answer`;
  - the list comes from local Codex `thread/list`, ACP `session/list` and
    project sessions.
- Changes are pushed on `/v1/sync/events`.
- Listing must not resume or load anything.

### 3.2 Each Atrium merges them

- The sidebar shows every device's conversations, newest first, each
  labelled with its device ("This computer", "studio", "laptop").
- The device filter is on the UI side.
- The overlay (§2) applies on top: your title, pin or hide follows the
  conversation to every device.

### 3.3 Reading a remote conversation

The UI asks the owner for it:

- `GET /v1/sync/threads/<backend>/<id>/snapshot` returns the transcript
  items (`atrium-core` `Item`s, with their stable ids) and a `cursor`.
- `GET /v1/sync/threads/<backend>/<id>/events?after=<cursor>` is a
  WebSocket of item upserts. It sends `rebuild` when the cursor is too old.

How the owner reads a conversation without taking it over:

- **Codex**: `thread/read`.
- **ACP agents**: replay the agent's own history only in a separate
  short-lived process, and only when no live process holds that session.
  If the conversation is open in this Atrium, serve it from the
  Workspace's transcript.
- **Claude Code specifically**: read its session file
  (`~/.claude/projects/<cwd slug>/<session>.jsonl`) when that is cheaper,
  through the same translator as live events, so ids match.

### 3.4 Offline cache

- Each device caches what it has read from other devices, under
  `data_dir/Atrium/sync/cache/<device>/<backend>-<id>.json`, with the
  cursor.
- It also prefetches the 20 most recently updated conversations of each
  device in the background.
- Cap: 500 MB, least-recently-used out.
- When the owner is offline, the conversation opens from the cache as
  read-only, with "studio is offline — last synced 14:02". Continuing it
  then means moving it (§5), which also needs the owner online. Say so.

## 4. Continuing a conversation from another device (default)

A follower sends what you do; the owner's Atrium executes it as if typed
there, so its own window shows it live too.

- `POST /v1/sync/threads/<backend>/<id>/turn` with
  `{ text, attachments, model?, effort?, mode? }`.
  - **Attachments** are uploaded first through the node's file API into
    the owner's `data_dir/Atrium/inbox/`.
- `POST …/steer`, `POST …/interrupt`.
- `POST …/approvals/<request_id>` and `POST …/answers/<request_id>`.
  - **First answer wins.** The others get `409` and show "answered on
    laptop".
  - Approvals and questions show on every device that has the
    conversation open, and as notifications on all of them.
- **New conversations.** When you start one on this computer, this
  computer owns it (local, as today). To start one on another device, pick
  the device in the agent picker. That device becomes the owner, and the
  request goes to `POST /v1/sync/threads` on it.
- **Fallback.** When the owner has only a standalone node, with no
  Atrium, keep today's path: this Atrium drives the agent process on that
  device through the node, and so becomes its writer. Show it as today,
  `Codex@studio`.

Everything in the side panel follows the conversation's owner, through the
node: the Changes list and git actions, the terminal, Run, and file
previews. Most of it already works through the node's file and process
APIs; check each, and list the gaps in the PR.

## 5. Move to this computer

An explicit action in the chat menu. It is allowed only when the agent is
idle on the owner and the owner is online. It runs in these steps; any
failure leaves both devices as they were.

1. **Find the folder here.** Match the conversation's repository on this
   computer by the normalized URL of its `origin` remote, falling back to
   its root commit, among this device's recent folders, projects and roots.
   - **Not found**: offer to clone it (`git clone <origin>` into a folder
     you choose).
   - **Not a git folder**: copy it through the node's file API after
     confirming the size, up to 200 MB. Over 200 MB is refused.
2. **Code.**
   - On the owner, record the working tree as a commit on
     `refs/atrium/move/<thread>`, using a temporary index exactly like
     checkpoints (`repo::checkpoint`). This leaves the owner's branch and
     index alone.
   - Write it as a `git bundle` that also carries the conversation's branch
     (chat worktrees use `atrium/<slug>`).
   - Fetch the bundle here through the node's file API, then `git fetch`
     from it.
3. **Apply here.**
   - If this checkout is clean and at the same base commit, check the
     moved tree out into it with a temporary index.
   - Otherwise create a chat worktree from that commit (`atrium/<slug>`),
     as `workspace_chat_trees.rs` does.
   - Take a checkpoint here first, so the move can be undone.
4. **Session.**
   - **Codex**: copy the rollout file into this machine's
     `~/.codex/sessions/` at the same relative path, then `thread/resume`
     here with the new `cwd`.
   - **Claude Code**: copy the session `.jsonl` into this machine's
     `~/.claude/projects/<slug of the new cwd>/`, then resume by session id.
   - **Other ACP agents**: if the agent supports loading the session on
     another machine, do the same. Otherwise start a new conversation here,
     seeded with the old one as Markdown (`export::to_markdown`), and say
     so.
5. **Ownership.**
   - The owner releases the session first: it closes its app-server
     thread, or its agent process for ACP. Before this step, verify that
     nothing still holds Codex's writer lock.
   - Record the move as an overlay op: `thread/<old ref>/moved_to = <new ref>`.
   - The old entry stays on the old device, read-only, labelled "Moved to
     laptop". The sidebar shows only the new one.

**Moving back** is the same action from the other side.

## 6. Projects across devices

- **Project identity**: the normalized `origin` URL plus the path inside
  the repository. Its id is a hash of those, falling back to the root
  commit.
- Each device maps the identity to its local checkout, if it has one.
- The `.atrium/` files already travel with git. What is live, such as
  running workstreams, the autopilot and approvals, belongs to the
  **scheduler device**.
- **Scheduler lease.** Only one device may run a project's scheduler and
  autopilot.
  - The overlay key `project/<id>/scheduler = { device, until }` is renewed
    every minute while it runs.
  - Another device takes over only after `until` has passed by 10 minutes,
    and only when the user confirms.
  - Before taking over, check the lease directly with the old device if it
    is reachable.
  - Two schedulers on one project must be impossible. Test it (§8).
- **Other devices** show the project overview from the scheduler
  device: lanes, notices and approvals, with actions sent as in §4. Without
  a local checkout, they open it remotely. With one, they also offer
  "Pull latest".

## 7. Who builds what

**Backend (this brief):**

- the embedded node and the device directory;
- pairing;
- `atrium-sync`;
- the `/v1/sync/*` endpoints;
- owner-side reading;
- the follower RPC;
- the cache;
- the move;
- the scheduler lease;
- the migration of settings.

**UI (built on the UI side):**

- the device label and filter in the sidebar;
- the first-run sharing consent and Settings → Devices (names, roots,
  pairing code);
- "studio is offline" banners and read-only state;
- "answered on laptop";
- "Move to this computer" with its progress and undo;
- "runs on studio" for schedules;
- the scheduler lease prompts.

New user-facing strings need a Chinese entry, as the i18n test requires.

## 8. Tests

**Unit tests in `atrium-sync`:**

1. Hybrid logical clock ordering, including clock skew and the same
   millisecond on two devices.
2. **Convergence.** With proptest, give three replicas random interleaved
   ops, deletes and duplicates; after a full exchange they hold identical
   state. Tombstones expire after 30 days.
3. The secret filter: a value under a secret key is never in an op.
4. Settings migration from an existing `settings.json`.

**Two Atriums on one machine** (`ATRIUM_DATA_DIR`, `ATRIUM_NODE_PORT` and
`ATRIUM_INSTALL_ID` overrides; loopback is trusted). Run these with
`mock-codex` and `mock-acp`:

5. A conversation on A appears on B within 2 s. A rename on B shows on A.
6. B reads A's conversation, and A's `mock-codex` sees only `thread/read`,
   never `thread/resume`. Assert it on the mock's request log.
7. **Follower turn.** B sends a turn; A executes it; both see the same
   items with the same ids, with no duplicates after B re-reads the
   history.
8. **Approval race.** An approval answered on A and B at once is accepted
   once; the other side gets "answered on".
9. **Offline.** Stop A; B opens the conversation from the cache, read-only.
   Start A and send a message on A; B catches up from its cursor without a
   rebuild.
10. **Move.**
    - A has uncommitted changes. Move the conversation to B: B's checkout
      or worktree has exactly A's tree.
    - The session resumes on B (`mock-codex` must support rollout files
      for this; extend it).
    - A's entry reads "Moved".
    - Undo the move on B.
11. **Scheduler lease.** Kill A while it runs the autopilot. B cannot take
    over before the lease expires, can take over after, and A refuses to
    resume as scheduler when it comes back.

**Real devices, once, written up in `docs/backend-acceptance-matrix.md`:**

12. Two computers with the same Tailscale owner: discovery, a follower
    turn, move both ways, and a laptop asleep and woken.
13. One device with a different owner: refused without pairing, then
    accepted after it.

## Backlog split

One PR each, in this order:

1. Embedded node, device directory, pairing.
2. `atrium-sync` and the settings migration.
3. Conversations from all devices: list, read, cache.
4. Continue from another device: follower turns and approvals.
5. Move to this computer.
6. Projects across devices and the scheduler lease.

## Done when

- `cargo test --workspace` is clean, including the two-Atrium tests (5–11)
  as integration tests.
- The real-device acceptance (12, 13) is recorded in the acceptance matrix,
  with anything not yet verified stated as such.
- No secret ever leaves its device. Test 3 checks this, and the PR states
  how it was reviewed.
