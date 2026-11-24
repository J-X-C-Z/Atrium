# T3 Code ⇄ Atrium differential harness

Runs one scenario twice — once under **T3 Code 0.1.11** and once under
**atrium-engine** — against the same `mock-codex` binary, reduces both runs to
the same normalized JSON lines, and diffs them. This is the acceptance harness
for the engine port (item 32): if the ported orchestration changes behaviour
that T3 exposes, the streams stop matching.

Python 3 standard library only (`run.py`).

```
tools/t3-diff/
  run.py                     the runner and T3 normalizer
  scenarios/*.json           one scenario per file
  out/<scenario>/            written by a run (gitignored)
```

Scenario kinds:

* `"kind": "engine"` (default) runs turns through
  `atrium_engine::runtime::CodexEngineRuntime`, the `AgentRuntime` seam the
  app selects. The driver receives the app's `AgentEvent`s, so the stream is
  the same shape the app sees.
* `"agent": "claude"` selects the Claude backend instead: the same engine
  kind runs `ClaudeEngineRuntime` over `claude-client` and `mock-claude`.
  T3 0.1.11 cannot drive a Claude account headlessly, so these scenarios
  always carry `"t3": "skip: <reason>"` and run engine-only.
* `"kind": "adapter"` drives `CodexSessionRuntime` directly for behaviour the
  T3 0.1.11 CLI cannot reach: mid-thread settings, `skills/list` /
  `config/read`, MCP elicitation outside full access, and `thread/read`
  versus `thread/resume`. Its `actions` are objects such as
  `{"op":"settings","model":"mock-1-pro","effort":"high","service_tier":"priority"}`,
  `{"op":"skills"}`, `{"op":"config"}`, `{"op":"read_snapshot"}`,
  `{"op":"hold_writer"}`, `{"op":"read_vs_write"}`, `{"op":"resume"}` and
  `{"op":"turn","text":"...","answer":"accept","runtime_mode":"approval-required"}`.
  With `"agent": "claude"` the adapter kind drives `ClaudeSessionRuntime`;
  its actions add `{"op":"commands"}` and `{"op":"skills"}`, and its traces
  are `claude.settings`, `claude.commands`, `claude.skills`, `claude.plan`,
  `claude.subagent`, `claude.rateLimits`, `claude.runtimeRequest` and
  `claude.responded`.
  Adapter scenarios carry `"t3": "skip: <reason>"`; the runner records the
  reason in `diff.txt` instead of pretending a T3 comparison happened.

`expect` may also carry:

* `stream_contains`: objects that must match (as a subset, string values by
  substring) some engine stream line.
* `provider_methods`: JSON-RPC methods the mock must have received. Both
  runs go through a small logging wrapper (`mock-wrap.py`), so T3's provider
  probe (`initialize`, `skills/list`, `config/read`, `model/list`, ...) and
  the engine's are checked against the same list. Per-side methods land in
  `t3-methods.json` and `engine-methods.json`. For Claude, the host's
  `control_request` subtypes (`initialize`, `set_model`,
  `set_permission_mode`, `interrupt`, ...) are collected the same way.

Scenarios:

| file | covers |
| --- | --- |
| `01-thread-lifecycle` | two turns, messages and final status |
| `02-approval-waiting` | a command approval leaves the turn waiting |
| `03-mid-thread-settings` | model, effort, service tier and approval applied to a loaded thread |
| `04-skills-and-config` | `skills/list` and `config/read` shapes |
| `05-elicitation` | MCP elicitation becomes a runtime request and is answered |
| `06-read-vs-write` | `thread/read` works while the writer lock is held; `thread/resume` reports in use |
| `07-provider-probe` | both sides call `initialize`, `model/list`, `skills/list`, `config/read`, `thread/start`, `turn/start` |
| `08-claude-thread` | Claude engine: two turns, echoed messages and final status (engine-only) |
| `09-claude-tool-approval` | Claude engine: a `can_use_tool` prompt leaves the turn waiting (engine-only) |
| `10-claude-adapter-events` | Claude adapter: slash commands, skills, TodoWrite plan, thinking, subagent lifecycle and mid-thread settings (engine-only) |
| `11-claude-usage-limit` | Claude engine: `rate_limit_event` + 429 result ends the turn failed (engine-only) |

## Build

```sh
export NO_PROXY='*'
export CARGO_TARGET_DIR="/path/to/target"   # optional; target/ in the repo also works

cargo build -p codex-client --bin mock-codex
cargo build -p claude-client --bin mock-claude
cargo build -p atrium-engine --bin atrium-engine-driver
```

`run.py` finds the binaries in `$CARGO_TARGET_DIR/{debug,release}` first,
then `<repo>/target/{debug,release}`, then `PATH`. Override with
`--mock-codex-bin`, `--mock-claude-bin`, `--engine-bin`, or
`MOCK_CODEX_BIN`, `MOCK_CLAUDE_BIN`, `ENGINE_BIN`.

T3 is looked up as `--t3-bin`, then `$T3_BIN`, then
`~/.t3/runtime/versions/0.1.11/t3`, then the newest installed version, then
`t3` on `PATH`. It needs no sign-in: the CLI talks to a local `t3 serve`.

## Run

```sh
python3 tools/t3-diff/run.py                 # every scenario
python3 tools/t3-diff/run.py 01-thread-lifecycle
python3 tools/t3-diff/run.py --no-t3         # engine side only, no T3 needed
python3 tools/t3-diff/run.py --keep 01       # keep scratch dirs for debugging
```

Exit codes:

| code | meaning |
| --- | --- |
| 0 | every stream matched and the expected final status held |
| 1 | a stream differed (see `out/<scenario>/diff.txt`) or the final status was not the expected one |
| 2 | environment/setup error: missing binary, T3 did not start, the mock is too old for T3's protocol gate, protocol failure or timeout |

`--no-t3` runs only the engine side; it still enforces
`expect.final_status`, writes `engine.jsonl`, and marks `diff.txt` as skipped.

## What each side does

**T3 side.** A scratch `T3CODE_HOME` gets
`userdata/settings.json` with a `mockcodex` provider instance whose
`binaryPath` is the built `mock-codex` and whose `homePath` is a scratch
`CODEX_HOME`. The runner then starts:

```sh
t3 project add <project> --title <name>
t3 serve --port <free> --host 127.0.0.1 --no-browser --base-dir <scratch> <project>
t3 thread start --provider mockcodex --model mock-1 --wait --json <project> "<first turn>"
t3 thread send  --wait --json <threadId> "<next turn>"
t3 thread show  --json --messages <n> <threadId>
```

`T3CODE_HOME` and `HOME` both point at the scratch dir, as in
`docs/t3-spike.md`. Exit codes 0/1/2 from `--wait` are accepted; 2 means the
run is waiting on a person, which is a scenario outcome, not an error.

**Engine side.** `target/.../atrium-engine-driver` reads the scenario JSON
from stdin and writes normalized JSON lines to stdout. Turns run through
`CodexEngineRuntime` (the `atrium_runtime::AgentRuntime` implementation) and
the ported Codex adapter over `codex-client`; item text and statuses come from
the same provider output T3 sees. With `"agent": "claude"` the driver runs
`ClaudeEngineRuntime`/`ClaudeAdapter` over `claude-client` and `mock-claude`
instead, and the T3 side is always skipped. The driver still owns the one-thread,
sequential-turns shape of a scenario; the orchestration queue itself is item
25. Adapter scenarios call the session runtime directly, as described above.

Both sides receive the same turn text. The driver adds T3's sender note
(`[T3 Code: this message came through T3 Code's tools, ...]`) before sending
it to the mock, because T3 does; the scripted mock echoes that first line into
its answer, so without the note the two assistant messages would differ for a
reason unrelated to orchestration. The normalizer removes the note from user
text.

## Normalized stream

One compact JSON object per line, in order:

```json
{"kind":"thread.created"}
{"kind":"run.started","ordinal":1}
{"kind":"item","ordinal":1,"role":"user","text":"say hi"}
{"kind":"item","ordinal":1,"role":"assistant","text":"..."}
{"kind":"approval.requested","ordinal":1}
{"kind":"run.finished","ordinal":1,"status":"completed"}
{"kind":"thread.finished","status":"completed"}
```

The schema and the text/status normalization live in
`crates/atrium-engine/src/diff.rs` (with unit tests); `run.py` mirrors them.
Keep the two in sync.

Comparison rules:

- run ordinals are 1-based; T3's `run:...:ordinal:N` suffix is used when
  present, otherwise the turn index;
- item roles are only `user` and `assistant`; reasoning, command execution,
  file changes, plans, web searches, approvals' payloads and all other items
  are dropped;
- text has CRLF/CR normalized to LF, T3's sender note removed from the front,
  and surrounding whitespace trimmed; interior whitespace is kept;
- statuses are lowercased (`completed`, `failed`, `waiting`, `cancelled`);
  `thread show` keeps a thread `running` while a person must answer, so a
  positive `pendingRequestCount` (or the run's own `waiting`) becomes
  `thread.finished ... waiting`;
- `approval.requested` is emitted before `run.finished ... waiting`.

## Intentionally ignored

Thread, run, turn and item ids; titles and previews; timestamps, durations and
timings; provider instance and model names; branch/worktree paths; hosted
`link` URLs; message counts; T3's `additionalContext` orchestration block;
token usage and rate limits; the exact order of non-message items; stderr and
log noise.

## Reading the result

`out/<scenario>/` contains:

| file | content |
| --- | --- |
| `t3.jsonl` | normalized T3 stream |
| `engine.jsonl` | normalized engine stream |
| `diff.txt` | unified diff (`MATCH` / `SKIPPED (--no-t3)` when clean) |
| `scenario.json` | a copy of the scenario that ran |
| `t3.log` | every T3 CLI command, exit code, stdout and stderr |
| `t3-results.json` | parsed `thread start/send` results |
| `t3-serve.log` | `t3 serve` output |
| `engine.stderr.log`, `engine.exit` | engine driver diagnostics |

Scratch dirs (`t3-home`, `codex-home`, `project`, `scratch-engine`) are removed
after a run unless `--keep`. They always live under `out/`, never in the repo
or the user's home. `t3-serve.log` has its connection string, pairing token
and app link redacted before it is written.

## Known limits

- The mock must satisfy T3's provider gate (`docs/t3-spike.md`): Codex
  user-agent ≥ 0.159.0, `thread/start` reply fields T3 decodes, and the
  `skills/list` / `config/read` stubs. Without those, T3 reports the run as
  `failed`; the runner classifies that as a setup error (exit 2) and prints
  T3's message.
- T3 0.1.11 has no MCP event stream, so the T3 side is folded from CLI
  results plus `thread show`. A scenario is therefore a sequence of completed
  (or waiting) turns, not an interactive session.
- Approval scenarios stop at the first pending request; the harness never
  answers it, matching `t3 thread ... --wait` with no person present.
- The provider process is not restarted when `settings.json` changes; each
  scenario uses a fresh `T3CODE_HOME` and `t3 serve`.
