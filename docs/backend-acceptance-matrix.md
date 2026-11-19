# Backend acceptance matrix

Review baseline: `37c86bfe` (the published source being reviewed). This matrix
separates implementation from validation environments. A workflow definition,
passing Linux test, cross-compilation, or older macOS run does not establish a
passing macOS CI run for this revision. The historical baseline is preserved below; the final section records the subsequent `30a5c029` isolated macOS acceptance and restoration to 12/20. No live GUI acceptance is implied.

## Original 20-package baseline: denominator unchanged

Source: [backend-status.md, Acceptance checklist and progress basis](backend-status.md#acceptance-checklist-and-progress-basis).
The historical **12/20 = 60%** was an unweighted acceptance-package count within
its stated isolated scope, not code rewritten or production readiness. This
review reopens package 12; the reviewed count is therefore **11/20 = 55%** on
that same denominator. No newly added tests or narrower platform scope increase
this count. Previously accepted packages retain their original limited scope;
this is not a fresh current-revision macOS runtime certification.

| # | Original package | Reviewed status at baseline |
|---|---|---|
| 1 | Safe TOML configuration and empty-root default | Accepted within original isolated scope |
| 2 | Native v1 metadata and client-compatible response shapes | Accepted within original isolated scope |
| 3 | Loopback token, Origin and Host boundaries | Accepted within original isolated scope |
| 4 | Process spawn/list/stop and opaque stdin/stdout | Accepted within original isolated scope |
| 5 | Sequence replay/gaps and attachment replacement | Accepted within original isolated scope |
| 6 | Bounded output/input and shutdown spawn admission | Accepted within original isolated scope; later bounds have separate evidence |
| 7 | Capability-relative path confinement | Accepted within original isolated scope |
| 8 | File list/stat/read/write/mkdir/changes | Accepted within original isolated scope |
| 9 | Scheduler roles, approvals and task claims | Accepted within original isolated scope |
| 10 | Capacity and offline/unknown distinction | Accepted within original isolated scope |
| 11 | Completion deduplication and unread report recovery | Accepted for receipt persistence/recovery, **not remote delivery acknowledgement** |
| 12 | Parent wakes and preservation of edit drafts | **Reopened: early-read failure boundary below** |
| 13 | Complete process-tree termination | Partial; Unix same-group tests do not contain escaped descendants; Windows runtime unverified |
| 14 | Real cross-device Tailscale identity and reconnect | Pending real-device acceptance |
| 15 | Service-manager installation lifecycle | Partial: generation/fault fixtures, no actual user-service installation |
| 16 | Cross-platform discovery and Magpie | Partial: mock/isolated coverage, not complete supported-platform runtime acceptance |
| 17 | Live GPUI plus real Codex/ACP workflow | Partial: native remote-client mock flows are not GUI/real-provider acceptance |
| 18 | Crash/fault-injection durability acceptance | Partial: targeted faults covered; remote kickoff/delivery ambiguity remains |
| 19 | Performance/load measurements and tuning | Partial: reproducible isolated measurements, not full remote/platform workload acceptance |
| 20 | Release packaging and supported-platform smoke tests | Pending complete supported-platform delivery acceptance |

### Why package 12 is reopened

At this baseline, `Workspace::with_stream_notices` in
`crates/atrium-app/src/workspace_project.rs` calls `project.mark_read` while
constructing a composed message, before transport acceptance, and ignores the
result of `project.save`. `deliver_notices_at` returns success after dispatching
`send_to_thread`, without receiving a transport acknowledgement. A failed send
can therefore leave a notice treated as read and suppress its future wake.
Draft isolation and wake predicates have useful tests, but do not close this
failure boundary. The [headless hierarchy test](hierarchy-mock-validation.md)
marks notices read after its successful mock turn; it does **not** exercise or
repair this Workspace ordering. Receipt recovery remains separately bounded as
described in [project-receipt-recovery.md](project-receipt-recovery.md).

## Separate macOS-first contract view: 16 functional packages

This is a distinct, explicit planning denominator, not a replacement or
renormalization of the original 20. **No overall completion percentage is
assigned to this matrix.** Test counts are not functional-package percentages.

Legend:
- **Implemented** describes source behavior, not platform certification.
- **Isolated** means temporary roots, owned processes, mocks, no provider account
  or user-service installation. “Partial” preserves the stated limitation.
- **Pending** means the required evidence is not established here.
- **N/A** means that environment is not a separate acceptance requirement for
  that package. It does not imply the package has passed.
- The CI column is specifically `37c86bfe`: [run 44471871205](https://github.com/J-X-C-Z/Atrium/actions/runs/44471871205) completed successfully, including workspace and native mock tests. This certifies only its isolated coverage, not later changes.
- Historical native Mac results in `backend-status.md` do not certify later
  changes. “Pending current” below does not erase those historical results.

| # | Functional contract | Implementation | Linux isolated evidence | macOS CI at `37c86bfe` | Real Mac acceptance | Second device | Trace |
|---|---|---|---|---|---|---|---|
| 1 | Safe configuration and empty-root default | Implemented | Config/root fixtures | Passed isolated CI | Pending current | N/A | [Status](backend-status.md), [protocol](node-protocol.md) |
| 2 | Token, Origin, Host and identity boundaries | Implemented | Auth and protocol negative cases; tailnet mocked | Passed isolated CI | Pending current | Required, pending for actual tailnet identity | [Protocol](node-protocol.md), [status](backend-status.md) |
| 3 | Spawn/list/stop and Unix same-group cleanup | Implemented, escaped descendants excluded | Owned-child EOF/TERM/KILL and group fixtures | Passed isolated CI | Pending current | N/A | [Status](backend-status.md), [protocol](node-protocol.md) |
| 4 | Opaque stdio, replay, gaps and attachment replacement | Implemented | Native loopback/replay/replacement fixtures | Passed isolated CI | Pending current | Remote behavior additionally covered by package 14 | [Protocol](node-protocol.md), [resource bounds](node-resource-bounds.md) |
| 5 | Runtime identity, reconnect and RPC closure/cancellation | Implemented; no automatic command resurrection | Owned-node restart, fake handshake and RPC cancellation fixtures | Passed isolated CI | Pending current | Real interruption pending under package 14 | [Lifecycle](node-lifecycle-validation.md), [RPC cancellation](rpc-cancellation.md) |
| 6 | Shared input/output and WS admission/release bounds | Implemented; not a whole-RSS hard cap | Small-budget deterministic/loopback tests; separately versioned output pressure evidence | Passed isolated CI | Pending current | N/A | [Protocol admission](node-protocol.md#native-input-and-attachment-admission-bounds), [pressure](node-retention-pressure.md) |
| 7 | File APIs and capability-relative confinement | Implemented | File, symlink and path-swap fixtures | Passed isolated CI | Pending current | N/A | [Status](backend-status.md), [protocol](node-protocol.md) |
| 8 | Hierarchy, lead approval, task claims and availability/capacity | Implemented | Production model plus Codex/ACP mock transport; fixture drives ticks/files | Passed isolated CI | Live Workspace pending | N/A | [Hierarchy validation](hierarchy-mock-validation.md), [implementation](hierarchy-implementation.md) |
| 9 | Stable receipts, deduplication and reopen recovery | Implemented, bounded single-writer scope | Fault/model tests and reopened-project mock flow | Passed isolated CI | Pending current | N/A; concurrent writers not promised | [Receipt recovery](project-receipt-recovery.md) |
| 10 | Notice delivery and failure-safe read cursor | **Incomplete: early-read remains** | Model/mock success path does not cover failing live dispatch | Passed existing tests; early-read gap still present at baseline | Pending after repair | N/A for local correctness; remote failure under 14 | Early-read discussion above; [hierarchy limits](hierarchy-mock-validation.md) |
| 11 | Metadata fault recovery and explicit ambiguous-start handling | Implemented with limits; not a multi-file/global transaction | Targeted save/startup fault injection | Passed isolated CI | Live crash boundaries pending | N/A; multi-writer transactions excluded | [Status durability](backend-status.md#durability-hardening-second-pass), [receipt recovery](project-receipt-recovery.md), [startup notices](startup-failure-notices.md) |
| 12 | macOS discovery, Magpie and agent adapters | Implemented paths; real integrations unverified | Mock discovery/adapter responses | Passed isolated CI | Actual installed-agent discovery pending | N/A for localhost discovery | [Status](backend-status.md), [quickstart](node-macos-quickstart.md) |
| 13 | Actual launchd install/start/stop/uninstall lifecycle | Implemented commands and recovery logic | Pure generation and injected launchctl-result/file-race fixtures only | Passed fixtures; no actual installation | **Pending actual lifecycle** | N/A | [Launchd recovery](launchd-recovery.md) |
| 14 | Real second-device identity and interrupted connection recovery | Implemented primitives | Loopback and simulated failures only | Passed isolated coverage only | Required, pending | **Required, pending** | [Lifecycle](node-lifecycle-validation.md), [protocol](node-protocol.md) |
| 15 | Existing desktop through real Codex/ACP backend workflow | Backend/client paths implemented; full flow unverified | Actual clients to repository mocks; no live Workspace scheduling | Passed mock coverage only | **Pending GUI and real-provider workflow** | N/A for initial same-device flow | [Hierarchy limits](hierarchy-mock-validation.md), [quickstart](node-macos-quickstart.md) |
| 16 | macOS deliverable build/package and reproducible performance/resource acceptance | Build/demo tooling and measurements exist; full current deliverable not accepted | Historical versioned benchmarks/load reports and helper tests | Passed build/tests; no release-package delivery | Current package smoke/performance pending | N/A for initial macOS package | [Benchmark](node-benchmark.md), [bounded load](node-bounded-load.md), [pressure](node-retention-pressure.md), [quickstart](node-macos-quickstart.md) |

A later evidence update should name the exact commit, environment, command/run
link, outcome and remaining exclusions. Keep pressure/baseline binary hashes
attached to their original reports: the intentionally different retained-output
budgets are not a fair speedup benchmark and must not be attributed to a newer
untested binary. Do not mark actual launchd, real provider accounts, or two-device
acceptance complete merely because isolated macOS CI succeeds.

## Follow-up delivery changes, separately gated

The subsequent notice journal, typed ACP outcomes and Workspace hooks have separate model/helper and mock validation described in [notice-delivery-recovery.md](notice-delivery-recovery.md) and [notice-dispatch-hooks.md](notice-dispatch-hooks.md). They are not part of the `37c86bfe` baseline above. Their exact-head macOS app compilation/CI is recorded below. Live Workspace acceptance remains a separate requirement under original package 17; it was not part of package 12's original isolated debounce/edit scope. The explicit uncertain-delivery resolution remains a backend API, not a newly wired frontend control.

The `30a5c029` follow-up completed full macOS CI successfully in [run 44474442418](https://github.com/J-X-C-Z/Atrium/actions/runs/44474442418), including app/workspace compilation/tests and explicit native mocks. This closes its compile-check gap, not live Workspace ticker/window acceptance or the frontend resolution-control gap. The separate opt-in ticker candidate is unpublished and has not run. Under the original isolated macOS scope, package 12 is restored: the original debounce/edit-draft isolation tests and five production dispatch-helper tests passed in the app's 58-test run, alongside the fault-model and native-client checks. Current accepted count is therefore **12/20 = 60% on the unchanged isolated checklist**, not code rewritten or GUI acceptance. The 11/20 review at the `37c86bfe` baseline remains recorded above. Real ticker/window/provider acceptance remains package 17, still incomplete.


## Cross-platform follow-up

The backend runs as `atrium-node` on all three platforms. The existing macOS
desktop continues to exercise Workspace scheduling. Linux and Windows CI run
the node, core, and transport clients without GPUI.

| Platform | Observed validation | Remaining validation |
|---|---|---|
| macOS arm64 | `bc42cf6` passed 229 workspace tests, three native scripted-agent flows and the temporary real launchd install/restart/uninstall test. Release package smoke passed locally. | Interactive desktop with provider accounts and second-device Tailnet acceptance. |
| Debian 13 arm64 | `bc42cf6` executed in a Linux VM using hash-verified binaries built on macOS. 59 node unit, four protocol, 32 model, and three scripted-agent tests passed. The real systemd 257 user lifecycle installed, started and removed the temporary unit and refused to remove an unrelated unit. | Ubuntu x64 CI and package smoke for the review branch. |
| Windows x64 | Strict node Clippy for the MSVC target passed through `3ccf212`. Windows fixtures cover Job descendants, batch launch, path handling and Task Scheduler definition validation. | Native Windows CI and the scheduled-task install/start/uninstall cycle in an interactive Windows session. |

The Workspace timer tests cover confirmed acceptance and a rejected RPC whose
notice remains unread and held after reopening. The subprocess crash test kills
a writer after reservation and verifies that a fresh process blocks replay. The
workstream chat offers explicit recovery through **Review notice delivery**.

The original package run [44522702311](https://github.com/J-X-C-Z/Atrium/actions/runs/44522702311)
passed the archive smoke and bounded-load cases. Its second paced run verified
all 1,314 sends, below the offered-load floor of 1,024 per client. The functional
checks run on every applicable PR and the paced test retains its threshold as a
manual workflow input. See [packaging instructions](node-only-package.md).

Windows registrations use `AtriumNode-<user SID>` and an interactive user token.
Sites with an older fixed-name `AtriumNode` registration should uninstall it using
its original binary before installing the per-user task. Linux runs under a
systemd user manager and requires an explicit administrator/user decision to
enable lingering. The archive scripts use the current native build target.
