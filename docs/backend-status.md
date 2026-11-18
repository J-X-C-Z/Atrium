# Backend implementation status

Based on repository commit `c7e121ff67c4f4d690af4d42873b5631cf77f642`.

## Node

`cargo run -p atrium-node -- serve --config /absolute/path/config.toml`

The node is a Rust/Tokio/Axum executable, not a wrapper around Python. It implements
protocol v1 device metadata, process listing/spawn/stop, opaque stdio WebSockets,
sequence replay and gaps, attachment replacement, bounded stdin/output buffering,
and root-confined file operations. Loopback and discovered Tailscale addresses are
bound individually; wildcard listeners are never used. Optional bearer authentication
also applies to WebSocket upgrades. Non-loopback identity checks fail closed on
Tailscale lookup failure. Browser Origin headers are rejected to prevent drive-by
use of ambient loopback access.

An absent configuration uses **no roots**, so metadata can be queried but file and
process APIs cannot access the home directory. Configure explicit roots first.
`keep_output_bytes` defaults to 128 MiB per process in addition to
`keep_output_lines`. At most 128 tracked process records are admitted. Stdin has
an independent bounded queue; slow attachments are detached rather than blocking
child output. Long lines are supported up to 64 MiB before JSON encoding.

`config path`, `install`, `uninstall`, and `status` are explicit commands. Service
installation is never performed merely by running tests or starting `serve`.
Service definitions exist for launchd, systemd user, and Windows Task Scheduler.
These commands have not been installed/validated against real user services.

### Contract choice and limitations

The process shutdown timing follows **node-protocol.md**, the declared contract:
close stdin, wait 2 seconds, TERM, wait 5 more seconds, KILL. The older implementation
brief's immediate TERM / 2-second wait is not used.

- File roots constrain file APIs and process starting directories. They are **not
  an execution sandbox** for an agent, which runs as the node user.
- File I/O is capability-relative (cap-std), including after path resolution, to
  prevent a symlink swap escaping the selected root. Directory listing does not
  follow symlinks; changes skips them. Directory traversal has a 100,000-entry bound.
- Unix direct-child cleanup and normal process-group stopping are implemented.
  On Unix, WNOWAIT retains the leader as an identity anchor until remaining
  process-group members are terminated, then reaps it. Leader exit gives the
  group TERM and a 100 ms grace before KILL, adding roughly 100 ms normal exit
  latency. This depends on exclusive child ownership and the locked Tokio
  reaper behavior; external waitpid(-1) reapers are unsupported. Descendants
  deliberately escaping the group with setsid/setpgid are not contained.
  Windows uses suspended creation, Job assignment before resuming the primary
  thread, and an anonymous non-inheritable kill-on-close Job. Windows code and
  its Job/EOF fixture tests cross-compile, but have not run on Windows. Therefore this
  slice remains experimental until cross-platform runtime acceptance.
- Output and process tables survive a client disconnect, **not node daemon restart**.
- Tailscale authentication uses its CLI, not the optional LocalAPI acceleration.
  Cross-device acceptance, changing tailnet membership, and service-manager
  integration have not been exercised against real devices.
- Magpie discovery probes only localhost in the background, with redirects
  disabled and bounded time/response size; mock tests cover valid/invalid replies.
  `discover_magpie=false` disables it for isolated tests. Claude's documented npx fallback is
  selected only when the adapter is missing and both claude and npx exist; no
  package is fetched during discovery.
- No real provider account or agent was used by the backend tests.

## Validation

`CARGO_TARGET_DIR=target-node cargo test -p atrium-node`

The integrated run passed 31 unit tests and one native-process/loopback
integration test. Two native mock-agent integration tests are ignored in the
ordinary workspace run and explicitly executed separately (2/2 passed). The latter covers missing token denial, browser Origin and hostile Host denial, malformed JSON,
file root confinement, native process input/output, sequence replay, attachment
replacement, owner filtering, and clean EOF stop. It uses only temporary roots
and `/bin/cat`, not real accounts. This is not an exhaustive security audit.

The project scheduler lives in existing backend/model files; no `ui/` files
are part of this change. Its tests are listed below.

## Scheduler validation

The integrated scheduler passed all 38 app tests and app Clippy. It now ticks
all projects, applies hierarchy/approval/availability/capacity rules, claims tasks
before launch, persists completion reports, resumes unread completion notices,
and debounces parent wakes. Background sends preserve the user's edit draft
and never invoke edit/revert. An orphan Running claim after a crash is failed
with a notice rather than automatically rerun. Existing project file persistence
is not a multi-file transaction. Live GPUI + Codex/ACP acceptance is still pending.

The existing `agent-rpc` ignored native-node acceptance test passed against the
new server using only a temporary root and cat agents (not mock-node). The node
then exited normally. Service installation and real tailnet identity testing
remain unrun. Strict node Clippy (`--all-targets -- -D warnings`) passed after the final
shutdown admission and EOF-to-TERM regression tests. The final workspace run
passed **116 tests**, with **4 ignored** environment-dependent tests and no
failures. Existing vendored Ely GPUI deprecation warnings and block future
compatibility warnings remain; this is not a warning-free workspace claim.
The process tests also reject oversized stdin without reserving queue bytes.
The SIGKILL escalation and naturally exiting leader with same-group
descendants now have passing tests. Escaped descendants remain out of scope.

## Acceptance checklist and progress basis

This is an **unweighted work-package acceptance count**, not percentage of code
rewritten, elapsed time, or a guarantee of production readiness. A package is
accepted only within the stated isolated macOS test scope. Partial packages do
not count as accepted. The checklist is a delivery planning baseline, not an
assertion that every upstream requirement has equal cost.

| # | Package | Status / evidence |
|---|---|---|
| 1 | Safe TOML configuration and empty-root default | Accepted: config/fs tests |
| 2 | Native v1 metadata and client-compatible response shapes | Accepted: native protocol/client acceptance |
| 3 | Loopback token, Origin and Host boundaries | Accepted: auth + protocol negative tests |
| 4 | Process spawn/list/stop and opaque stdin/stdout | Accepted: cat process/native tests |
| 5 | Sequence replay/gaps and attachment replacement | Accepted: ring + native replay/4001 tests |
| 6 | Bounded output/input and shutdown spawn admission | Accepted: budget/oversize/shutdown tests |
| 7 | Capability-relative path confinement | Accepted: symlink and routed-parent-swap failure tests |
| 8 | File list/stat/read/write/mkdir/changes | Accepted: fs/native tests; documented symlink policy |
| 9 | Scheduler roles, approvals and task claims | Accepted: model tests and app compile |
| 10 | Capacity and offline/unknown distinction | Accepted: scheduler tests incl offline-slot regression |
| 11 | Completion deduplication and unread report recovery | Accepted: scheduler tests |
| 12 | Parent wakes and preservation of edit drafts | Accepted: debounce/edit isolation tests |
| 13 | Complete process-tree termination | Partial: Unix tests pass; Windows Job cross-compiles but needs runtime acceptance |
| 14 | Real cross-device Tailscale identity and reconnect | Pending: CLI implementation only, no real-device acceptance |
| 15 | Service-manager installation lifecycle | Partial: rendered definitions tested, no actual installations |
| 16 | Cross-platform discovery and Magpie | Partial: Magpie mock-tested; Linux/Windows runtime untested |
| 17 | Live GPUI plus real Codex/ACP workflow | Partial: native-node mock client turns pass; GUI/real accounts untested |
| 18 | Crash/fault-injection durability acceptance | Partial: four filesystem fault-injection groups pass; live-session crash ambiguity remains |
| 19 | Performance/load measurements and tuning | Partial: two isolated release baseline runs; concurrency/CPU/remote load remain |
| 20 | Release packaging and supported-platform smoke tests | Pending: local debug build only |

Current accepted packages: **12 / 20 = 60% on this checklist only**. This is not
60% of code rewritten. Upstream contains a mock node, not a complete production
node of comparable source size, so dividing new Rust LOC by upstream LOC would
not provide a meaningful rewrite percentage. Report changes to this denominator
explicitly instead of silently inflating completion.

## Durability hardening (second pass)

Project metadata and task status use same-directory atomic replacement with file
fsync and Unix parent-directory fsync. Read failures are propagated rather than
replacing unreadable files with empty contents. Session binding must persist
before kickoff; failed launch recovery retains its Workstream evidence until
all required writes succeed. Notices append rather than overwriting concurrent
agent additions. This is not a multi-file transaction or global exactly-once
protocol: a crash between session binding and remote kickoff has an ambiguous
remote outcome, so the scheduler does not blindly replay it.

Linux aarch64 cross-target `cargo check` passes on the Mac build host; this
is compile-only validation, not a Linux runtime or service-manager test.


Native node → existing remote Codex/ACP clients → repository mock agents now
passes two explicit integration tests, including approval/permission responses,
streaming output and complete turns. These are ignored by ordinary workspace
tests because they require mock binaries built first; run the commands at the
head of `crates/atrium-node/tests/agents.rs`. They do not count as GUI or real
account acceptance.

Release microbenchmark evidence, sample counts, raw results and binary hash are
in [node-benchmark.md](node-benchmark.md). No relative speedup is claimed. The
[source inventory](backend-change-inventory.md) reports physical/nonblank Rust
lines separately from acceptance counts; there is no comparable production
backend source denominator for a rewrite percentage.

`.github/workflows/node.yml` prepares macOS backend tests with a pinned official
checkout action, read-only repository permission and no persistent checkout
credential. Local validation is not a claim that remote CI has passed; consult
the pull request's check results. Windows runtime work is deferred. No workflow
installs a user service, changes tailnet settings, or invokes real agents.


## Linux runtime verification

An independent x86_64 Linux cloud checkout verified the source handoff archive
and patch SHA-256 and applied it to the exact baseline. Native node tests passed:
31 unit tests, one native protocol test, and both explicit mock-agent remote
client tests. The mock fixture now locates binaries through Cargo's
`CARGO_BIN_EXE_atrium-node`, rather than assuming a fixed test executable layout.
Linux GUI/system dependencies (ALSA, xkbcommon and Wayland) were absent; no system
packages, service registrations or network settings were changed to bypass that
limitation. Windows remains cross-compile-only until a Windows runtime is
available. A Rust 1.100 deprecation was addressed with an equivalent atomic CAS
loop compatible with the workspace's Rust 1.95 MSRV.

Final Linux recheck after the CAS/fixture fixes: 31 unit + 1 protocol + 2
explicit mock-agent tests pass, and node `clippy --all-targets -- -D warnings`
passes on Rust/Cargo 1.100 (Debian 13, x86_64). The two changed files were
byte-for-byte SHA-256 matched against the Mac checkout:

- `src/procs.rs`: `0a3be648cb91fa43578cdc93d9f12421dd03a623fd687e0ff5d73c860d80cc32`
- `tests/agents.rs`: `8884ddb7282ce506f00bd133521ea854074e549506bada0598bf4fa2c3000b66`

The final Mac full workspace again passes 116 tests with 4 ignored; both
native mock-agent tests were also explicitly rerun and passed separately.


## Current delivery scope: macOS first

The owner subsequently asked to defer Windows and prioritize macOS. Windows
runtime work is paused; its code remains explicitly unverified on a real Windows
host. The prepared CI now runs **macOS only**, including full workspace tests,
strict node lint, and both native mock-agent client flows. No service installation,
network/security change, or user-owned GPUI implementation is part of this change.

The earlier 20-package table is the broader cross-platform planning baseline,
not the completion percentage of this narrower macOS delivery. Verified macOS
handoff gates are: native protocol/security, Unix lifecycle tests, confined file
operations, scheduler/fault tests, native remote-client mock turns, workspace/lint
regression, and a reproducible release microbenchmark. Publication/review evidence
will be recorded separately. Real provider-account, GUI and second-device tailnet
acceptance are still not claimed.
