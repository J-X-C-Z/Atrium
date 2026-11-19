# atrium-node protocol, version 1

`atrium-node` is a small daemon on each device of the user's tailnet. It lets
Atrium, on another device, run the agents installed there: it starts an agent
process and bridges its stdin and stdout to a WebSocket, so Atrium speaks the
agent's own protocol (Codex `app-server` JSON-RPC, or the Agent Client
Protocol) end to end. The node never parses those messages.

It also gives Atrium what it needs around the agents: what the device has,
its folders, and the files of the projects kept there.

This document is the contract between Atrium (the client) and the node. The
reference client is `crates/agent-rpc/src/node.rs`; a reference node, small
and incomplete on purpose (no auth, no persistence), is
`crates/agent-rpc/src/bin/mock_node.rs`. A node passes when Atrium's tests
pass against it (see [Acceptance](#acceptance)).

## Transport and security

- HTTP/1.1 on port **7381** by default. Plain HTTP: traffic between tailnet
  devices is already encrypted by WireGuard.
- The node listens **only** on its Tailscale addresses (100.x.y.z and the
  fd7a:115c:a1e0::/48 one) and on loopback. Never on `0.0.0.0`.
- **Who may connect**: for every connection from a tailnet address the node
  asks the local Tailscale daemon who it is, through the LocalAPI
  `GET /localapi/v0/whois?addr=<ip>:<port>` (the `tailscale` Go package's
  `LocalClient.WhoIs` does the same). It is allowed when
  - the peer's `UserProfile.LoginName` is the node owner's own login
    (the login of the node's own device, from `/localapi/v0/status`), or is
    listed in `allow_users` in the config, or
  - the peer device carries a tag listed in `allow_tags`.

  Anything else is `403 {"error":"not_allowed"}`. Loopback is allowed (for a
  client on the same device and for tests).
- Optional shared secret: with `token = "..."` in the config, every request
  must also carry `Authorization: Bearer <token>`, else `401`. Off by default.
- Every error answer is JSON: `{"error": "<code>", "message": "<words>"}`
  with a fitting status. Codes used below: `not_found`, `bad_request`,
  `not_allowed`, `unauthorized`, `outside_roots`, `agent_unavailable`,
  `spawn_failed`, `gone`.

## Configuration

`~/.config/atrium-node/config.toml` (Windows: `%APPDATA%\atrium-node\config.toml`).

```toml
port = 7381
name = "studio"                   # shown in Atrium; the hostname when unset
roots = ["~/code", "~/work"]      # folders Atrium may browse, read, write and run agents in
allow_users = []                  # other tailnet logins allowed besides the owner's
allow_tags = []                   # device tags allowed, such as "tag:atrium"
token = ""                        # optional bearer token
keep_output_lines = 20000         # output kept per process for reattaching
detached_timeout_secs = 86400     # a process nobody attached to for this long is stopped

# Agents. Detected on PATH when not listed; listing one overrides detection.
[agents.codex]
command = ["codex", "app-server"]
protocol = "codex-app-server"
[agents.claude]
command = ["claude-agent-acp"]    # falls back to ["npx", "-y", "@agentclientprotocol/claude-agent-acp"] when only `claude` is installed
protocol = "acp"
[agents.deepseek]
command = ["dsh", "--profile", "acp"]
protocol = "acp"
[agents.antigravity]
command = ["agy", "acp"]
protocol = "acp"
```

Detection must find binaries the way a login shell would: on macOS a
launchd service does not get the shell's `PATH`, so ask the login shell
(`$SHELL -lc 'echo $PATH'`) once at start and add the usual install folders
(Homebrew, `~/.npm-global/bin`, `~/.volta/bin`, `~/.local/bin`, `~/.cargo/bin`).

Paths: `~` expands to the home folder. A path is **inside the roots** when,
after resolving `..` and symlinks, it is a root or below one. Every path in
a request must be inside the roots, else `403 outside_roots`.

## Device

### `GET /v1/node`

```json
{
  "name": "atrium-node",
  "version": "1.0.0",
  "protocol": 1,
  "device": { "name": "studio", "hostname": "studio", "os": "macos", "arch": "aarch64" },
  "roots": ["/Users/me/code", "/Users/me/work"],
  "home": "/Users/me",
  "agents": [
    { "id": "codex", "name": "Codex", "protocol": "codex-app-server", "version": "0.169.0", "available": true },
    { "id": "claude", "name": "Claude Code", "protocol": "acp", "version": "0.107.0", "available": true },
    { "id": "deepseek", "name": "DeepSeek Harness", "protocol": "acp", "version": null, "available": false,
      "reason": "dsh is not installed" }
  ],
  "magpie": { "version": "1.42.0", "port": 3425 }
}
```

- `protocol` is this document's version. Atrium refuses a node whose
  `protocol` it does not know.
- `agents` lists every known agent, available or not; `reason` says why one
  is not. `version` is what `<binary> --version` printed, trimmed, when it
  answered within 3 seconds; else `null`.
- `magpie` is present when magpie answers `GET http://127.0.0.1:<port>/api/hello`
  on this device (`port` from `~/.config/magpie/settings.json`, 3425 when
  unset); `null` otherwise.
- Answer within one second: cache versions and detection, refresh them in
  the background.

## Processes

A process is one agent the node started for Atrium. It outlives the
WebSocket it was attached through: a laptop that sleeps or loses its network
reattaches and gets what it missed.

### `POST /v1/procs`

```json
{ "agent": "claude", "cwd": "/Users/me/code/aurora", "owner": "6f1c…", "args": [], "env": {} }
```

- `agent`: an id from `/v1/node`. Unknown → `404 not_found`; not available
  → `409 agent_unavailable`.
- `cwd`: inside the roots, an existing folder. Missing → `400 bad_request`.
- `owner`: an opaque id of the Atrium install; kept and listed, so a client
  finds its own processes again.
- `args`: appended to the agent's command. `env`: added to its environment.

Start the command in `cwd`, with stdin, stdout and stderr piped and the
login-shell `PATH`. Answer `201`:

```json
{ "id": "p-7Kq2x", "agent": "claude", "cwd": "/Users/me/code/aurora", "owner": "6f1c…",
  "pid": 41234, "started_at": 1795003412, "attached": false, "exit": null }
```

A command that cannot start → `500 spawn_failed` with the OS error.

### `GET /v1/procs`

The processes still running, and those that ended less than
`detached_timeout_secs` ago, in the same shape (`exit` is
`{"code": <int|null>, "signal": <string|null>, "at": <unix seconds>}` once ended).
`?owner=<id>` keeps one owner's.

### `DELETE /v1/procs/{id}`

Stops it: close stdin, then SIGTERM after 2 seconds, then SIGKILL after 5
more (on Windows, terminate). `204`; `404` for an unknown id.

### `GET /v1/procs/{id}/attach?after=<seq>` — WebSocket

Upgrades to a WebSocket. Every frame is a **text** frame holding one JSON
object.

Node → client:

| Frame | Meaning |
| --- | --- |
| `{"seq": 17, "line": "<one line of stdout, without the newline>"}` | Output. `seq` counts lines from 1 and never repeats for this process. |
| `{"seq": 18, "stderr": "<one line of stderr>"}` | Diagnostics; shares the counter. |
| `{"gap": {"from": 1, "to": 4120}}` | Lines in that range were dropped (evicted by `keep_output_lines`, per-process `keep_output_bytes`, or node-wide `total_output_bytes`); sent first, before the replay, when the client asked for lines no longer kept. |
| `{"seq": 4500, "exit": {"code": 0, "signal": null}}` | The process ended; the node closes the socket after it. |

Client → node:

| Frame | Meaning |
| --- | --- |
| `{"line": "<one line>"}` | Written to stdin, followed by `\n`. |

Rules:

- `after` is the last `seq` the client has. The node first sends every kept
  frame with a larger `seq`, in order, then live frames as they come.
  `after=0` (or none) replays everything kept; `after=-1` skips the backlog
  and sends only new output.
- Output is split on `\n` only; a line may be long (a megabyte or more). Do
  not cap frame size below 64 MiB.
- One attachment at a time: a new attach closes the previous socket with
  close code 4001 (`replaced`).
- Send a WebSocket ping every 15 seconds; a client that misses two is
  detached (the process keeps running).
- Attaching to an ended process replays what is kept, then the `exit` frame.
  Unknown id → plain `404` before the upgrade.
- Writing to stdin never blocks the socket's reader: queue writes per
  process.
- A process nobody is attached to keeps running and keeps its output, until
  `detached_timeout_secs` passes with no attachment; then it is stopped as
  `DELETE` does.

## Files

For projects kept on the device: Atrium reads and writes the project's
`.atrium/` folder through these, and the folder picker browses with `list`.
Every `path` is absolute (or starts with `~`) and inside the roots.

| Route | Answer |
| --- | --- |
| `GET /v1/fs/list?path=P` | `{"path": P, "entries": [{"name", "dir": bool, "size", "modified"}]}`, sorted with folders first, hidden entries (a leading `.`) left out unless `&hidden=1` |
| `GET /v1/fs/stat?path=P` | `{"exists": bool, "dir": bool, "size", "modified"}` (`modified` in Unix milliseconds) |
| `GET /v1/fs/read?path=P` | the file's bytes, `application/octet-stream`; `404` when missing; files over 16 MiB → `413` |
| `PUT /v1/fs/write?path=P` | body is the new content, written to a temporary file beside it and renamed over; parent folders are created; `204` |
| `POST /v1/fs/mkdir?path=P` | creates it and its parents; `204` |
| `GET /v1/fs/changes?path=P&since=<ms>` | `{"now": <ms>, "changed": [<paths under P modified after since>]}`, recursing at most 3 levels, for polling a project's atrium |

## Acceptance

Run Atrium's node tests against a node on this machine:

```bash
ATRIUM_TEST_NODE=http://127.0.0.1:7381 cargo test -p agent-rpc --test node -- --ignored
```

With the node's config listing the mock agents from this repository, so no
real agent and no account is needed:

```toml
roots = ["/tmp"]
[agents.codex]
command = ["<repo>/target/debug/mock-codex", "app-server"]
protocol = "codex-app-server"
[agents.claude]
command = ["<repo>/target/debug/mock-acp", "--name", "claude"]
protocol = "acp"
```

The same tests run against `mock-node` in CI, so the two must agree.

### Native node output retention budget

`keep_output_bytes` retains its legacy per-process effective ceiling (at least
64 MiB + 1,024 bytes to fit maximum-sized frames). The optional
`total_output_bytes` config adds a node-wide ceiling, default 128 MiB, shared by
all running and retained exited processes. It must fit one 64 MiB payload plus
128 bytes of accounted frame overhead. Each retained stdout/stderr frame counts
its UTF-8 payload bytes plus 128 bytes; this is an accounting bound, not an RSS
hard limit. Partial reader buffers, replay copies, JSON serialization, stdin and
network buffers are not covered by this retention budget.

When full, the largest retained-output consumer loses its oldest frame first;
equal consumers use stable process registration order to break ties. Per-process
line and byte ceilings still apply. Eviction removes a contiguous sequence
prefix, does not reset sequence numbers, and replays report the existing `gap`
frame. A request for only new output (`after=-1`) does not replay a gap.

One fixed-shape terminal frame per process is retained separately from stdout/
stderr bytes, so budget pressure cannot hide process completion (including an
ended process attached with `after=-1`). Removing the final process handle
releases its retained-output allocation; ending a process alone retains its
history until the existing retention cleanup. Output is not persisted to disk.

### Optional node lifecycle identity (v1-compatible)

The native node emits `instance_id` in `/v1/node`, spawned process responses,
and process listings. It is a fresh random UUID for each server start, stable
across metadata refreshes, and is never persisted. It identifies a runtime,
not an authenticated user or proof that an OS process was terminated.

WebSocket attach accepts the optional `X-Atrium-Instance-Id` header (nonempty,
at most 128 bytes). After normal authentication, a mismatch returns HTTP 409
with `{"error":"node_restarted","message":"..."}` before process lookup.
A matching identity with an unknown process still returns 404 `not_found`.
Successful upgrades include the current identity in the same response header,
so identity validation and attachment do not have a separate GET/upgrade race.

New clients pin identity per process/attachment, never from a mutable latest
node-info cache. An attached link retains its pin across transient reconnects.
A confirmed `node_restarted` terminates that link with a clear `Gone` reason;
queued stdin is discarded, no command is spawned automatically, and old stdout
is not restored or persisted. Network errors alone remain retryable.

`NodeClient` keeps a bounded FIFO convenience cache of at most 256 process
identities learned from spawn/list responses; active links have independent
pins. `attach_with_instance` accepts an explicit previously retained identity
for older IDs or a newly constructed client. This API does not persist anything
or alter UI storage. If an identity was never learned, evicted, or lost with a
client restart, the client cannot retrospectively distinguish a prior restart
from generic process absence. A successful new handshake learns an identity
for that link, not an identity for unknown past runtime state.

Old clients omit the header and ignore additive response fields. Old servers
without lifecycle fields/headers retain their prior generic-404/reconnect
behavior for clients without a pinned identity. If an already pinned link
receives an upgrade without identity, it stops with an explicit identity-support
error instead of sending queued input to an unverified runtime; this is not
reported as a confirmed restart. No durable tombstones or work-content storage are introduced.

Input delivery across disconnects is not exactly-once: a connection can fail
after sending bytes but before the client knows whether the agent consumed
them. Existing queued-input retry/send-failure behavior is unchanged by the
lifecycle-identity extension, and callers must not infer reliable delivery or
automatically retry consequential commands from a reconnect alone. Output
replay covers only retained sequences and may report a `gap`.
