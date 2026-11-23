# T3 Code headless spike

Date 2026-11-23. T3 Code 0.1.11 (npm `t3@latest`), Linux x64, driven against Atrium's `mock-codex`. Verdict: **an external program can drive T3 headless.** The CLI path works with no sign-in. The MCP path works too, but needs a headless OAuth step that is not in the docs. MCP Events are not in 0.1.11.

## Install and run

- `npm view t3` is a 9 KB launcher (`bin/t3.mjs`). On first run it downloads `t3-0.1.11-linux-x64.tar.gz` (about 490 MB unpacked) from `github.com/pingdotgg/t3code/releases`, checks it against `SHA256SUMS`, and unpacks it to `$T3CODE_HOME/runtime/versions/0.1.11/`. The download went through the proxy with no changes; first run took about 8 s. `T3CODE_RELEASE_BASE_URL` overrides the host.
- The executable is a single 171 MB binary (Node-based) plus `lib/`, `client/` and `node_modules/`.
- Data dir: `T3CODE_HOME` (or `--base-dir`); state lives in `$T3CODE_HOME/userdata/` (`statev2.sqlite`, `settings.json`, `secrets/`, `server-runtime.json`). Worktrees go in `$T3CODE_HOME/worktrees/`. I also set `HOME` to a scratch dir.
- Headless server: `t3 serve --port 3790 --host 127.0.0.1 --no-browser [<cwd>]`. It prints a pairing URL and token on start and serves HTTP and WebSocket on one port. `server-runtime.json` records pid, host and port. The CLI subcommands (`t3 thread ...`, `t3 project ...`, `t3 auth ...`) find the running server through that file (same `T3CODE_HOME`), with no sign-in. `t3 up` sets up remote access and a background service for long-running use; not tried.
- Startup: HTTP answers about 1.3 s after launch (migrations 0.4 s). RSS of the server process: about 470 MB idle, 445 to 495 MB around turns. T3 also keeps one provider process per instance alive (mock 4 MB; a real Codex would add its own memory). A small `t3-resource-monitor` child uses 3 MB.

## Provider config (point T3 at a custom Codex binary)

No env var. Provider instances are declared in `$T3CODE_HOME/userdata/settings.json`, which is hot-reloaded (a few seconds, no restart). A running provider process is NOT restarted by a settings change or wrapper edit; restart `t3 serve` after changing the binary's behaviour.

```json
{"providerInstances":{"mockcodex":{"driver":"codex","displayName":"Mock Codex",
  "config":{"binaryPath":"/path/to/codex-or-wrapper",
            "homePath":"/path/to/codexhome",
            "launchArgs":"-c mcp_servers.atrium.url=\"http://127.0.0.1:9999/mcp\""}}}}
```

`binaryPath` is the codex executable; `homePath` becomes `CODEX_HOME`; `launchArgs` is appended to `codex app-server` (verified: argv was `app-server -c thread_unload_delay_secs=0 -c mcp_servers.atrium.url=http://...`). Other drivers (claudeAgent, etc.) exist under the same `providerInstances` map; the provider instance id (here `mockcodex`) is what `--provider` takes.

### What T3 needs from the Codex binary (mock gaps)

T3 runs `<bin> --version` and `<bin> app-server`, then sends `initialize`, `account/read`, `skills/list`, `model/list`, `config/read`, `thread/start`, `thread/name/set`, `turn/start`. `mock-codex` ignores argv (`--version` just prints `mock-codex: ready`). I did NOT edit the mock; I put a wrapper in front (`codex-wrap.sh`: answers `--version`, tees stdin/stdout to logs, pipes stdout through `fix.py`). To run the unmodified mock under T3 these changes are needed in `crates/codex-client/src/bin/mock_codex.rs`:

1. `initialize` result `userAgent` must carry a Codex version >= 0.159.0, e.g. `"codex_cli_rs/0.160.0"` (currently `mock-codex/0.0.0` makes every run fail: "Codex 0.0.0 does not work with this T3 Code release. Update Codex to >=0.159.0").
2. `thread/start` (and resume) response must also include `approvalPolicy` (`"never"`), `approvalsReviewer` (`"user"`) and `sandbox` (`{"type":"dangerFullAccess"}`); the `thread` object must include `cliVersion` (string), `projectId` (null ok) and `source` (`"appServer"`). Without these T3 fails with `Invalid payload for method 'thread/start' during 'decode-payload'`.
3. Optional: `skills/list` and `config/read` return -32601 today. T3 tolerates it for running threads, but its provider probe marks the instance unhealthy ("probe failed: mock-codex does not implement skills/list") and `orchestrator_capabilities` then lists `models: []` for it. `--provider mockcodex --model mock-1` still works. Suggested stubs: `skills/list` -> `{"data":[{"cwd":"...","skills":[],"errors":[]}]}`, `config/read` -> `{"config":{}}` (shapes not verified against T3's decoder).
4. `--version` handling: T3 calls `<bin> --version`; the mock prints a non-version line. It did not block runs here, but the wrapper answers `codex-cli 0.130.0`.

Note the user message reaches Codex prefixed: `[T3 Code: this message came through T3 Code's tools, not from the user typing in this chat.]\n\n<text>`, and each `turn/start` carries `additionalContext` (`t3_code_orchestration_01`, a "T3 Code orchestration" block telling the agent to prefer `delegate_task`, call `orchestrator_capabilities`, etc.). Atrium's own agent instructions will sit beside that.

## CLI (works, no auth)

Run from anywhere with `T3CODE_HOME` set. Project argument is an id or a path inside the project (`.` = cwd); `thread list` takes `--project` (no positional).

```sh
t3 project add /path/to/repo --title spike
# -> Added project 412c0294-...-aa0f-9b68ab19da9b (spike) at /path/to/repo.   (plain text, no --json)

t3 thread start --provider mockcodex --model mock-1 --wait --json . "say hi"
# {"threadId":"mcp:e41601d6-...","runId":"run:thread:mcp%3A...:ordinal:1","status":"completed","reply":"...","error":null}
t3 thread send  --wait --json <threadId> "now say bye"        # same shape, ordinal:2
t3 thread show  --json --messages 2 <threadId>
t3 thread list  --json [--project .]
```

- `thread start` without `--provider` uses the default instance `codex` (not installed here, so the run fails with exit 1). `--option id=value` needs `--provider` and `--model`.
- Exit codes with `--wait`: 0 completed, 1 failed, 2 waiting on a person (verified with the mock's approval-style prompt "run ls and edit": status `waiting`, `reply:null`, exit 2). Sending to a waiting thread also returned 2.
- `--worktree main --branch spike-wt` worked: T3 ran `git worktree add` into `$T3CODE_HOME/worktrees/<repo>/spike-wt-<hash>` and the thread's `branch`/`worktreePath` show it.
- Thread ids look like `mcp:<uuid>`. Every thread JSON has a `link` to `https://app.t3.codes/threads/...` (hosted URL, only meaningful with T3 Connect).

`thread show --json` (trimmed):
```json
{"thread":{"threadId":"mcp:...","projectId":"412c...","title":"say hi","createdBy":"agent","creationSource":"mcp",
  "status":"completed","latestRunId":"run:...:ordinal:2","activeRunId":null,"providerInstanceId":"mockcodex","model":"mock-1",
  "runtimeMode":"full-access","interactionMode":"default","branch":"main","worktreePath":null,
  "runCount":2,"itemCount":9,"pendingRequestCount":0,"archived":false,"settled":false,"blockedOnThreadWait":false,
  "parentThreadId":null,"createdAt":"...","updatedAt":"..."},
 "messages":[{"role":"user","runId":"run:...","text":"now say bye"},{"role":"assistant","runId":"run:...","text":"..."}]}
```
`thread list --json` is a bare array of the same thread objects (without `activeRunId`/`branch`), newest first. Status values seen: `completed`, `failed`, `waiting`, `preparing`, `starting`.

## MCP over HTTP (works, but needs an OAuth step)

- `t3 auth session issue --json --ttl 1h --label x [--scope ...]` and `--token-only` work and print a bearer token. **That token is rejected by `/mcp`** (401 `invalid_mcp_credential`): `/mcp` only accepts its own `mcp-client` OAuth audience. The bearer token is for the REST/WebSocket API instead.
- Headless OAuth (PKCE, no browser), scripted in `mcp_auth.py` (stdlib Python, in the spike folder). Discovery: `/.well-known/oauth-protected-resource/mcp`, `/.well-known/oauth-authorization-server`.
  1. `t3 auth pairing create --json --ttl 5m` -> `credential`.
  2. `POST /oauth/mcp/register` `{"client_name":"atrium","redirect_uris":["http://127.0.0.1:9/cb"],"token_endpoint_auth_method":"none"}` -> `client_id` (stateless). The redirect URI is never contacted; loopback http is allowed.
  3. `POST /oauth/mcp/decision` `{"authorization":{"response_type":"code","client_id","redirect_uri","code_challenge","code_challenge_method":"S256","state","resource":"<base>/mcp"},"decision":{"_tag":"pairing-code","access":"full-access","code":"<credential>"}}` -> `{"redirectTo":"...?code=..."}`. `access` is one of `read-only`, `approval-required`, `auto-accept-edits`, `auto`, `full-access`.
  4. `POST /oauth/mcp/token` (form-encoded: `grant_type=authorization_code`, `code`, `redirect_uri`, `client_id`, `code_verifier`, `resource`) -> `{"access_token":"...","token_type":"Bearer","expires_in":2592000,"scope":"orchestration:read orchestration:operate settings:write source-control:write"}`.
  There is no refresh grant (`grant_types_supported: ["authorization_code"]`): re-run the flow every 30 days. Full-access MCP clients get `settings:write` and `source-control:write` as well as orchestration.
- Calls: `POST <base>/mcp` with `Authorization: Bearer <access_token>`, `Content-Type: application/json`, `Accept: application/json, text/event-stream`. `initialize` (protocolVersion 2025-06-18) -> `Mcp-Session-Id` header, which must be echoed on later calls; reply is plain JSON. Server capabilities: `logging`, `tools {listChanged:true}`, `completions`. `serverInfo` = `T3 Code 0.1.11`. `resources/list` and `prompts/list` return empty. `GET /mcp` (SSE stream) returns 405.
- Verified calls (structuredContent shapes, trimmed):
  - `t3_project_list {}` -> `{"projects":[{"id","title","workspaceRoot",...}],"nextCursor":null}`
  - `t3_thread_launch {"projectId","title","message","modelSelection":{"instanceId":"mockcodex","model":"mock-1"}}` -> `{"threadId","link","projectId","modelSelection","runId","status":"preparing"}`. Only `title` is required; outside a thread you must pass `projectId` (or `scratch:true`). Also takes `runtimeMode`, `interactionMode`, `workspaceStrategy` (worktree), `clientRequestId` (idempotent retry).
  - `t3_thread_wait {"threadId","timeoutMs":30000}` -> `{"threadId","runId","status":"completed","timedOut":false}` (returned after about 2 s, i.e. when the turn ended). This is a blocking long-poll and is the usable "turn ended" signal in 0.1.11.
  - `t3_thread_read {"threadId","limit":4}` -> `{"thread":{...},"recentRuns":[{"runId","ordinal","status","startedAt","completedAt"}],"items":[{"position",...}]}`; views `messages` (default) or `activity`.
  - `t3_thread_send {"threadId","message","mode":"auto"}` -> `{"messageId","runId","status":"starting","delivery":"started"}`.
  - `t3_pending_request {"action":"list|read|respond"}` answers questions only; it cannot approve a permission request.
- `orchestrator_capabilities` lists provider instances with models and per-model options (e.g. Claude `effort` low..max, `fastMode`); it reported the built-in `codex` instance as not installed and `mockcodex` as probe-failed (see gaps).
- `delegate_task` is not in an outside agent's tool list (T3 says use `t3_thread_launch`).

### MCP Events: not available in 0.1.11

`docs/user/outside-agents.md` on `main` describes `io.modelcontextprotocol/events` (turn ended, waiting on a person, webhooks via `T3CODE_MCP_WEBHOOK_ALLOWED_HOSTS`). The 0.1.11 server does not advertise it and answers `events/list`, `events/poll`, `events/subscribe` with -32601 `Method not found`. There is no events string in the binary. It is probably in the nightly (`0.1.12-nightly.20261123.5138`, npm tag `nightly`); I did not download it (490 MB, disk is tight). Until then, an Atrium service can use `t3 thread ... --wait` / `t3_thread_wait` per thread, or poll `t3_thread_list` (status `waiting` = waiting on a person; `pendingRequestCount`).

## Extra MCP servers for provider sessions

- T3 injects its own `/mcp` into every Codex session it starts, per thread, in the `thread/start` request `config` (captured in the wrapper log), with a per-thread bearer token:
  ```json
  "config":{"mcp_servers.t3-code":{"url":"http://127.0.0.1:3790/mcp","tool_timeout_sec":3900,"http_headers":{"Authorization":"Bearer ..."},"disabled_tools":["t3_thread_wait","delegate_task"]},
            "mcp_servers.t3-code-direct":{"url":".../mcp","enabled_tools":["t3_thread_wait","delegate_task"],"omit_tools_from":["code_mode","deferred"]},
            "shell_environment_policy.set.T3CODE_THREAD_ID":"mcp:...","tools.update_plan.enabled":true, ...}
  ```
  So agents inside T3 threads can call `t3_thread_launch` and `delegate_task` themselves (this overlaps with Atrium's controller/lead delegation).
- T3 has no documented per-thread hook for third-party MCP servers: the `config` object is built by T3. Our options, all verified or read from the binary:
  1. Codex: `launchArgs` on the provider instance (`-c mcp_servers.atrium.url=...`) applies to every thread of that instance. Verified in argv.
  2. Codex: `homePath` sets `CODEX_HOME`, so a dedicated instance can carry its own `config.toml` with `[mcp_servers.atrium]`; T3 calls `config/read` (with `cwd`), so Codex's normal layered config is in play. Not verified with a real Codex.
  3. Claude: the Claude adapter runs the Claude SDK with setting sources (user, project, local; only project and local when a custom config dir is set), so `~/.claude.json`, project `.mcp.json` and `.claude/settings.json` apply. Not run here (no Claude account).
  Per-thread distinct servers would have to be keyed on a per-project `.mcp.json` / project-scoped config, or on the `T3CODE_THREAD_ID` env var that T3 sets in shell commands.

## Blockers and open questions

- MCP Events missing in latest stable: need nightly (or a newer release) to test them.
- `/mcp` auth is OAuth-only; Atrium must implement the pairing-code flow above (or issue pairing codes via `t3 auth pairing create` on the host) and renew every 30 days. The OAuth flow is undocumented for headless use and may change.
- Idle RSS about 470 MB for the server alone.
- First run downloads about 490 MB; pin `T3CODE_RELEASE_BASE_URL` or pre-seed `runtime/versions/` for offline installs.
- Not tested: real Codex/Claude providers, approvals through MCP, `schedule_task`, workflows, T3 Connect, the background service via `t3 up`, multiple projects concurrency, thread deletion/archive, the Claude MCP path.
- `thread/start` decode failures show up only as run `failed` with the error string; Atrium should surface `error` from the JSON.

## Appendix: MCP tools/list (59 tools, 0.1.11, full-access client)

- `orchestrator_capabilities`: List providers, models, options, and runtime modes for `delegate_task`. Short list. `search` finds o...
- `t3_workflow_stop`: Stop a running Claude workflow in a thread. `subagentId`: coordinator from `t3_thread_read` 'activit...
- `schedule_task`: Create scheduled work that runs with no turn active, from `prompt` and object `schedule`. `scheduled...
- `list_scheduled_tasks`: List scheduled tasks. Omit `projectId` for this thread's project. Pass a `scheduledTaskId` to `sched...
- `delete_scheduled_task`: Delete one scheduled task by `scheduledTaskId`. It stops at once. To pause instead, use `schedule_ta...
- `t3_thread_list`: List threads newest first, in one project or with `allProjects`. Page with `cursor`.
- `t3_thread_read`: Read any thread by id. View `messages` (default) or `activity` (all items). Newest first; older: `be...
- `t3_thread_update`: Rename a thread (`action:'rename'` with `title`) or regenerate its title. Omit `threadId` for this t...
- `t3_thread_send`: Send a message to any thread. The target cannot run with broader modes than yours. `mode:'auto'` sta...
- `t3_thread_wait`: Wait for a thread's run to finish. No `runId`: latest. Idle: at once. On timeout, with `next` end yo...
- `t3_thread_interrupt`: Interrupt a top-level thread's running turn. No `runId`: newest running turn. No effect when idle. F...
- `run_scheduled_task_now`: Run one scheduled task now. Needs a full-access or default caller. Returns its thread, or null if no...
- `t3_thread_search`: Search active threads by title, branch, PR, project, then message text. Skips subagent threads. `pro...
- `t3_thread_configure`: Read a thread's provider, model, and modes, or set its provider, model, and options with `modelSelec...
- `t3_thread_transfer`: Move context between threads. Omit `threadId` for this thread. fork: copy it from `sourcePoint` (def...
- `t3_thread_organize`: Pin, snooze, settle, archive, or mark a thread unread. Omit `threadId` for this thread. `snooze` nee...
- `t3_pending_request`: Pending user questions in a thread. Omit `threadId` for this thread. list: their ids; approvals are ...
- `t3_queue`: Manage a thread's queued messages. Omit `threadId` for this thread. list: send order, paged by `curs...
- `t3_attachment_prepare_upload`: Get an upload URL. POST the file's exact bytes to `uploadUrl` (else this server's `relativeUrl`), th...
- `t3_attachment_discard`: Discard a pending upload (not a sent attachment).
- `t3_thread_send_attachments`: Send uploaded attachments as a new message to a thread. No retry key. Target modes cannot exceed you...
- `t3_thread_launch`: Create one new top-level thread, bound to its workspace before the agent starts. Task in `message`. ...
- `t3_project_list`: List projects. Pages can shift.
- `t3_project_read`: Read one project: workspace and saved scripts.
- `t3_project_create`: Register a project folder. `createWorkspaceRootIfMissing` creates it; a registered folder is rejecte...
- `t3_project_update`: Update one project's settings. Omitted fields are unchanged.
- `t3_project_delete`: Delete one project. Nonempty needs `force: true`. Folder on disk stays.
- `t3_project_clone`: Clone a repository (`destinationPath` new or empty). Register after with `t3_project_create`, with t...
- `t3_environment_read`: Read this server's identity and preferences. Providers and models: `orchestrator_capabilities`.
- `t3_environment_preferences_update`: Update environment-wide preferences. Live full-access or default caller thread only. Omitted fields ...
- `t3_theme_read`: Read T3 Code's own themes and the guide to restyle it: color roles and CSS selectors. Call before `t...
- `t3_theme_set`: Restyle T3 Code itself, only when the user asks. With `theme`, publish it as `themeId` (same id upda...
- `t3_worktree_list`: List branch refs and checkout paths for a thread's workspace. Omit `threadId` for this thread. Detac...
- `link_pull_request`: Link a PR to this thread. T3 shows its status and settles the thread on merge. For several PRs, use ...
- `link_pull_requests`: Link several PRs to a thread. Use it instead of one `link_pull_request` per PR. URLs that are not PR...
- `unlink_pull_request`: Remove a PR link from this thread. Succeeds with `wasLinked=false` when it was not linked.
- `unlink_pull_requests`: Unlink a thread's PRs whose last known state is in `states`. Use it instead of one `unlink_pull_requ...
- `list_thread_pull_requests`: List a thread's PRs, with last known state and stack order (bottom to top). Not an end-of-work check...
- `watch_pull_request`: Watch an open PR for this thread, linking it first if needed. T3 wakes you on failed checks, passed ...
- `unwatch_pull_request`: Stop watching a PR for this thread. It stays linked.
- `write_pull_request_guides`: Write review guides for up to 20 PRs (the tour in the PR Guide tab). Returns once runs start. Each r...
- `read_pull_request_guide`: Read a PR's stored review guide and run state. `guide` is null when none exists. Never starts a run.
- `pull_request_read`: Read a PR's state, checks, review comments, or diff. Use instead of gh or cafe.
- `pull_request_list`: List PRs in this thread's repository, recently updated first.
- `pull_request_create`: Open a PR from this thread's pushed branch and link it, or return its open PR.
- `pull_request_update`: Edit a PR's title, body, labels, or reviewers, close or reopen it, or set ready (false: back to draf...
- `pull_request_respond`: Reply to, resolve, or reopen a review thread. Without reviewThreadId, comment on the PR.
- `pull_request_review`: Submit a review: approve, request changes, or comment, with optional line comments.
- `pull_request_merge`: Merge a PR. Refuses if the head moved from expectedHeadSha (summary.headSha).
- `ci_logs`: Read the tail of a PR's failing check logs, or one named check's. Use instead of gh run view.
- `ci_rerun`: Re-run a PR's failing checks, or one named check.
- `list_issues`: List issues in a project's tracker, newest updated first. Pass the returned nextCursors as cursors f...
- `read_issue`: Read an issue: body, state, labels, assignees, recent comments and linked pull requests. Pass its ur...
- `create_issue`: File an issue in a project's tracker.
- `comment_on_issue`: Post a comment on an issue.
- `set_issue_state`: Move an issue to one of its states. read_issue lists them under states.
- `link_issue`: Link an issue to a thread so T3 Code shows it beside the thread. Link every issue you work on or fil...
- `unlink_issue`: Remove an issue link from a thread. The issue itself is not changed.
- `list_thread_issues`: List the issues linked to a thread.
