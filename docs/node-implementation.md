# Building atrium-node

A brief for implementing `atrium-node` in Rust. The contract is
[`node-protocol.md`](node-protocol.md); this file says how to build it, what
to watch out for, and how to know it is done. Atrium (the client) already
exists and is tested against `mock-node`; the real node must behave the same.

## What it is

One small binary, `atrium-node`, run as a user service on each device. It:

1. answers `GET /v1/node` with the device and its agents;
2. starts agent processes and bridges their stdio to WebSockets, keeping
   them running and their output buffered while nobody is attached;
3. serves a few file operations inside configured roots;
4. lets in only the owner's tailnet devices.

It never parses the agents' own messages. It must not need Atrium, an
account or the internet to start.

## Suggested layout

A new repository (or `crates/atrium-node` in Atrium's workspace):

```
src/
  main.rs        CLI: `atrium-node serve` (default), `install`, `uninstall`, `status`, `config path`
  config.rs      config.toml: load, defaults, `~` expansion, roots canonicalised once
  detect.rs      login-shell PATH, agent detection, `--version` probes (cached, refreshed every 10 min)
  tailscale.rs   own addresses and login; `whois` for a peer address
  auth.rs        middleware: loopback, tailnet whois, optional bearer token
  procs.rs       process table, output ring buffer, stdin writer task, reaper
  attach.rs      the WebSocket handler: replay, live frames, input, ping, replacement
  fs.rs          list / stat / read / write / mkdir / changes, roots check
  magpie.rs      `GET /api/hello` probe for the `magpie` field
  service.rs     launchd plist, systemd user unit, Windows service or Task Scheduler entry
```

Crates: `tokio` (full), `axum` with the `ws` feature, `serde`/`serde_json`,
`toml`, `tracing` + `tracing-subscriber`, `dirs`, `anyhow`/`thiserror`,
`nix` (Unix signals and process groups), `windows-sys` on Windows only,
`tokio-util` (line codecs), `uuid` or `rand` for process ids.

## Notes per part

### Binding and addresses

- Find the device's tailnet addresses at start with `tailscale ip -4` and
  `tailscale ip -6` (or the LocalAPI status). Bind one listener per address
  plus `127.0.0.1`. If Tailscale is down at start, bind loopback only and
  retry the tailnet addresses every 30 seconds; rebind when they change.
- Never bind `0.0.0.0` or `::`.

### Who may connect (`tailscale.rs`, `auth.rs`)

- The simplest portable way is the CLI: `tailscale whois --json <ip>:<port>`
  gives `UserProfile.LoginName` and `Node.Tags`. Cache answers per peer
  address for a minute. The LocalAPI is faster where reachable
  (`/var/run/tailscale/tailscaled.sock` on Linux; on macOS the GUI app's
  API needs the port and token from its `sameuserproof-*` file), so use it
  when available and fall back to the CLI.
- The owner is the login of this device (`tailscale status --json`,
  `Self.UserID` → `User[<id>].LoginName`).
- Loopback is allowed without whois. Everything else that is not the owner,
  not in `allow_users`, and carries no tag in `allow_tags` gets `403`.
- With a `token` configured, check `Authorization: Bearer` on every route
  including the WebSocket upgrade, in constant time.

### Processes (`procs.rs`)

- Spawn with `tokio::process::Command`, `kill_on_drop(false)`, stdin,
  stdout and stderr piped, the login-shell `PATH`, `cwd` from the request.
  On Unix put the child in its own process group (`process_group(0)`) so a
  stop reaches what it started (npx, node, shells).
- Read stdout with a line codec **without a length limit below 64 MiB**:
  agents send long JSON lines (file contents, diffs). Split only on `\n`;
  strip a trailing `\r`.
- One sequence counter per process across stdout, stderr and exit.
- Keep the last `keep_output_lines` frames in a `VecDeque`; remember the
  first `seq` still kept so a late `after` can be told it has a gap.
- Live frames go to the attached socket through a `tokio::sync::broadcast`
  (or a `watch` on the newest seq plus reads from the buffer); a slow
  socket must never block reading the child's stdout. If a socket falls too
  far behind, close it; the client reconnects with `after` and catches up
  from the buffer.
- Stdin: an `mpsc` channel per process drained by one task, so writes are
  ordered and a full pipe never blocks the WebSocket reader.
- Stop (`DELETE`, idle timeout, node shutdown): close stdin, SIGTERM to the
  group, wait 2 s, SIGKILL, then record `exit`. On Windows use a Job Object
  so the whole tree ends.
- Keep ended processes listed until `detached_timeout_secs` after their end,
  so a client that comes back sees how they ended.
- On node shutdown (SIGTERM from the service manager) stop every process.

### Attaching (`attach.rs`)

- Look up the process **before** upgrading: an unknown id is a plain 404.
- After upgrade: if `after` is below the first kept `seq - 1`, send the
  `gap` frame; then the kept frames past `after` in order; then live ones.
  Subscribe to live frames before reading the buffer, and skip seqs already
  sent, so nothing falls between the two.
- `after=-1`: start from the newest seq.
- A new attach replaces the old: close the previous socket with code 4001.
- Ping every 15 s; two missed pongs detach (the process keeps running).
- Inbound frames: `{"line": "..."}` → stdin queue. Ignore anything else.

### Files (`fs.rs`)

- Canonicalise every path (resolve `~`, `..` and symlinks) and check it is
  inside a canonical root. For a path that does not exist yet (a write),
  canonicalise its nearest existing parent and append the rest.
- `write`: write to `<name>.atrium-tmp-<random>` beside it, `fsync`, rename.
- `changes`: walk at most 3 levels, compare `modified` with `since`.

### Agents (`detect.rs`)

- Defaults when not configured: `codex app-server`; `claude-agent-acp`, or
  `npx -y @agentclientprotocol/claude-agent-acp` when only `claude` is on
  PATH; `dsh --profile acp`; `agy acp`.
- `available: false` with a `reason` when the binary is missing.
- Probe versions with a 3-second timeout, in the background.

### Service (`service.rs`)

- macOS: `~/Library/LaunchAgents/dev.atrium.node.plist`, `KeepAlive`,
  `RunAtLoad`, logs to `~/Library/Logs/atrium-node.log`. A user agent, not a
  daemon: the agents need the user's home and keychain.
- Linux: `~/.config/systemd/user/atrium-node.service`,
  `Restart=on-failure`; tell the user to `loginctl enable-linger` for it to
  run while logged out.
- Windows: a per-user Scheduled Task at logon (agents need the user's
  profile), or a service running as the user.

## Pitfalls

- macOS services get a bare `PATH`; agents installed with Homebrew, npm or
  Volta are then "not installed". Resolve the login shell's PATH.
- Agents write their own config and sessions under the user's home: run as
  the user, never as root.
- `npx` prints to stderr while it downloads; that is not an error.
- Do not buffer stdout in a `BufWriter` on the WebSocket side without
  flushing per frame: interactive agents stall.
- Do not treat a closed WebSocket as a reason to stop the process.

## Acceptance

1. `cargo test` of the node's own unit tests (path checks, ring buffer gaps,
   replacement, stop escalation).
2. Atrium's client tests against the real node, with the mock agents from
   Atrium's repository configured (see
   [node-protocol.md → Acceptance](node-protocol.md#acceptance)):

   ```bash
   cargo build --workspace --bins          # in Atrium: builds mock-codex and mock-acp
   ATRIUM_TEST_NODE=http://127.0.0.1:7381 cargo test -p agent-rpc --test node -- --ignored
   ```

3. Atrium itself against the node: start Atrium with
   `ATRIUM_NODES=127.0.0.1:7381`; the device appears under Settings →
   Devices, its agents appear in the agent picker under its name, a chat
   with its Codex runs a turn with an approval, and cutting the network for
   a few seconds (or `sudo pkill -STOP` the node for 5 s, then `-CONT`)
   shows "Reconnecting…" and then carries on with nothing lost.
4. From a second tailnet device: the device appears without adding it by
   hand, and a device logged in as another user is refused (403).
