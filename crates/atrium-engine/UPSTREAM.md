# Upstream: T3 Code

Ported from **T3 Code v0.1.11**, commit `59651c1761f6c63e9b6056839067066968e6a406`
(2026-11-20), MIT. The original notice is in [`LICENSE-T3`](LICENSE-T3).

Every ported file starts with a one-line header naming its T3 source path.
Keep this table current when a module lands or moves.

| `atrium-engine` module | T3 source | Item |
| --- | --- | --- |
| `src/contracts` | `packages/contracts/src` (base schemas, orchestration V2, model selection) | 20 |
| `src/store` | `apps/server/src/orchestration-v2/EventStore.ts`, `ProjectionStore.ts`, `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts`, `ThreadStream.ts`, `LiveStreamBudget.ts`, `threadHistoryPaging.ts`, `WireProjection.ts`, `packages/shared/src/orchestrationV2BoundedSnapshot.ts` | 21 |
| `src/orchestration` | `apps/server/src/orchestration-v2/Orchestrator.ts`, `EventSink.ts`, `EffectWorker.ts`, `runtimeLayer.ts` | 25 |
| `src/providers` | `packages/provider-core`, `packages/effect-codex-app-server`, `packages/provider-acp` | 22, 23, 24 |
| `src/usage` | `orchestration-v2/UsageLimitRecoveryWorker.ts`, limit schemas | 26 |
| `src/checkpoint` | `apps/server/src/checkpointing`, `orchestration-v2/Checkpoint*Service.ts` | 27 |
| `src/mcp` | `packages/contracts/src/orchestratorMcp.ts`, `apps/server/src/mcp` | 28 |

`boundedSnapshotTransport.ts` has no module of its own in v0.1.11; only its
HTTP-level test exists, backed by `threadHistoryPaging.ts` and the shared
bounded-snapshot helpers. The store-level snapshot and compaction cases are
ported in `src/store/stream_tests.rs`; the WebSocket/HTTP framing is skipped
with the transport.

### Ported outside `atrium-engine`

These live where Atrium's own code already does the job (the port folds into
one implementation instead of adding a second subsystem). Item 31 compared
T3's provider-native workflows with Atrium's own workflow runner
(`crates/atrium-workflow`) and ported the parts Atrium lacked into that runner
rather than adding a second engine. The same MIT notice covers these files.

| Atrium module | T3 source | Item |
| --- | --- | --- |
| `atrium-project/src/pr_watch` (`evaluate`, `sync_due`, `watch_message`, `watch_end_message`) | `apps/server/src/orchestration-v2/pullRequestWatch.ts`, `PullRequestSyncReactor.ts` (`isDue` cadence and failed-read backoff), `packages/contracts/src/threadPullRequest.ts` (watch fields) | 31 |
| `atrium-project/src/pr` (`Observation`, comment/review remarks, head SHA, mergeability) | `apps/server/src/pullRequest/pullRequestSyncKey.ts`/`pullRequestTarget.ts` shape only; `gh pr view` replaces T3's provider reads | 31 |
| `crates/atrium-app/src/schedule.rs` (schedule kinds, next-run and DST rules, structural comparison, run status, restart recovery) | `apps/server/src/scheduledTasks/Schedule.ts`, `packages/contracts/src/scheduledTask.ts` | 31 |
| `crates/atrium-app/src/workspace_schedules.rs` (startup recovery, one-run-at-a-time dispatch) | `apps/server/src/scheduledTasks/ScheduledTaskService.ts` | 31 |
| `crates/atrium-workflow/src/files.rs` | `apps/server/src/orchestration-v2/workflowFileRead.ts` | 31 |
| `crates/atrium-workflow/src/answers.rs` | `apps/server/src/orchestration-v2/workflowAgentAnswers.ts`, `workflowAgentTranscript.ts` | 31 |

The T3 unit tests for `evaluatePullRequestWatch` were ported alongside the
module in `atrium-project/src/pr_watch.rs`. Deliberately left out: T3's
per-file viewed state, change breakdown, linked-PR sources and stacks,
provider registry and merge-queue reads — Atrium has no UI or data path for
them, and `gh pr view` exposes no merge-queue removals.

## What is not ported

T3's web UI, Electron shell, mobile apps, relay, pairing and OAuth stay out;
Atrium keeps its own GPUI UI, `atrium-node`, projects, team, memory, workflows
and Office work. Running agents through T3 itself (`docs/t3-extension-plan.md`)
is paused.
