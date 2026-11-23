# Projects spread over several computers

Read `AGENTS.md` first, then `crates/atrium-project/src/spread.rs`.

## Decisions (made with the user)

- **One folder name everywhere.** Every computer has an **Atrium Workspace**
  folder, wherever it likes (`~/Atrium Workspace`, `D:\Atrium Workspace`).
- **Same path in every workspace.** A project sits at the same path inside
  each one, but each computer keeps only the folders it works on. No git
  syncs the computers.
- **One writer per folder.** The writer is the computer whose workstream
  owns the folder: its `scope`, with the device taken from its backend
  (`codex@win`). Other computers read the folder over the network, or copy
  it.
- **`.atrium/` stays on the scheduler.** The scheduler is the computer that
  runs the project. Agents elsewhere reach `.atrium/` only through the
  `atrium` tools.
- **Example.** The user's project:
  - `ui/` on Linux, with the controller (Claude Code);
  - `analysis/`, `perf/core/` and `perf/win/` on Windows (Codex);
  - `perf/mac/` on the Mac (Codex).

  The user's local agent sets the machines up, following
  `docs/atrium-workspace-setup.md`. Your work is the app.

## What is built

- **`spread.rs`:**
  - `WORKSPACE_DIR`.
  - `relative`, `join_on` and `workspace_in_roots`: path mapping, including
    Windows separators.
  - `device_of(backend)`.
  - `Spread::of(project)`: `owner_of(path)`, `foreign(stream, paths)`.
  - `briefing(project, stream, scheduler)`: the "Computers" section of a
    kickoff.
- **App settings:**
  - `workspace_dir`, and `atrium_workspace()` for this computer.
  - `device_workspaces`: overrides for other computers, by device id.
  - `Workspace::device_workspace(device)`: the override, else the node
    root named `Atrium Workspace`.
  - `Workspace::project_on_device(root, device)`.
  - Settings → Devices shows and sets this computer's folder, and shows each
    device's folder.
- **Scheduler (`start_stream`):**
  - A workstream whose backend is on another device starts in
    `project_on_device(..)`, not in a local path, and with no worktree. When
    that folder is unknown, the start fails with a clear reason.
  - Kickoffs of spread projects get `briefing(..)` appended.

## Yours: one PR

1. **The `atrium` tools for agents on other computers.**
   - Today `Workspace::mcp_servers` returns nothing for a remote backend, so
     a lead on `codex@win` cannot list tasks, notify, write status or finish
     a task. Serve the same MCP server over the network from the scheduler
     computer, for example as Streamable HTTP on the embedded node:
     `/v1/mcp?project=<root>&stream=<id>`.
   - Use the node's existing transport security: Tailscale identity or a
     paired token. Never listen beyond the tailnet and loopback.
   - Give remote sessions that server by URL. `McpServer` needs a `url`
     form; Codex and ACP both accept HTTP MCP servers.
   - The tools and their checks stay exactly as in `mcp.rs`.
   - Test: a mock remote session calls `whoami` and `finish_task` and
     changes the scheduler's task file.
2. **Kickoff wording for remote agents.** Kickoffs tell agents to edit
   `.atrium/...` files by path. For a remote stream, say to use the tools
   instead (`list_tasks`, `create_task`, `write_status`, `notify`,
   `finish_task`). Keep local kickoffs byte-for-byte as they are.
3. **Reading other computers' files.** Add three `atrium` tools, available
   in spread projects only:
   - `device_list(device, path)`;
   - `device_read(device, path)`: text, or a note for binary files;
   - `device_copy(device, path, to)`: copies a file or folder from another
     computer into the caller's own scope.

   `path` is relative to the project. Resolve it with `project_on_device`
   and go through the node's `/v1/fs/*` routes. Refuse:
   - paths outside the project;
   - `device_copy` into a path the caller does not own
     (`Spread::owner_of`);
   - copies over 200 MB.

   `device` is a device id, or the scheduler's name for the scheduler.
4. **One writer per folder, enforced softly.** When a remote session's
   `FileChange` touches paths that `Spread::foreign` reports:
   - post a notice from `atrium` to the stream and to the folder's owner,
     as crossings do today;
   - do not revert anything.
5. **The project overview shows where each lane runs.** Show the device
   name on the lane, from its backend. Add a zh string if it needs new
   text.

## Tests

- `spread.rs` already has tests for mapping, ownership and the briefing.
- Scheduler tests, GPUI-free where possible:
  - a remote lead starts in `D:\Atrium Workspace\photo` when the project is
    `~/Atrium Workspace/photo` here;
  - a project outside the workspace fails with the reason;
  - local leads are unchanged.
- MCP over the network: an unauthenticated caller from off the tailnet is
  refused, and a paired one works.
- Run the end-to-end check with `mock-node` standing in for "win". Its
  roots must include a folder named `Atrium Workspace`.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets`,
  `cargo fmt --all`.
