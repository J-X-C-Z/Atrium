# Upstream: T3 Code

Ported from **T3 Code v0.1.11**, commit `59651c1761f6c63e9b6056839067066968e6a406`
(2026-11-20), MIT. The original notice is in [`LICENSE-T3`](LICENSE-T3).

Every ported file starts with a one-line header naming its T3 source path.
Keep this table current when a module lands or moves.

| `atrium-engine` module | T3 source | Item |
| --- | --- | --- |
| `src/contracts` | `packages/contracts/src` (base schemas, orchestration V2, model selection) | 20 |
| `src/store` | `apps/server/src/orchestration-v2/EventStore.ts`, `ProjectionStore.ts`, `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts`, `ThreadStream.ts`, `LiveStreamBudget.ts`, `threadHistoryPaging.ts`, `WireProjection.ts`, `packages/shared/src/orchestrationV2BoundedSnapshot.ts` | 21 |
| `src/orchestration` | `apps/server/src/orchestration-v2/Orchestrator.ts`, `CommandPolicy.ts`, `EventSink.ts`, `EffectWorker.ts`, `QueuedRunOrder.ts`, `RuntimePolicy.ts`, `runtimeLayer.ts`, `ProviderSwitchService.ts`, `ProviderSessionTransitionPolicy.ts`, `RunFinalizationService.ts`; `packages/provider-core/src/server/subagentProjection.ts` | 25 |
| `src/providers` | `packages/provider-core` (`ProviderAdapter.ts`, `selectionTransition.ts`, `driver.ts`, `client.ts`), `apps/server/src/orchestration-v2/ProviderAdapterRegistry.ts`, `apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts`, `apps/server/src/provider/CodexMcpElicitation.ts`, `packages/effect-codex-app-server` (wire names only; the transport is Atrium's `codex-client`) | 22, 23, 24 |
| `src/runtime` | Atrium seam over `ProviderAdapterV2` (`atrium-runtime::AgentRuntime`); no single T3 file | 22 |
| `src/usage` | `orchestration-v2/UsageLimitRecoveryWorker.ts`, limit schemas | 26 |
| `src/checkpoint` | `apps/server/src/checkpointing`, `orchestration-v2/Checkpoint*Service.ts` | 27 |
| `src/mcp` | `packages/contracts/src/orchestratorMcp.ts`, `packages/contracts/src/project.ts`, `packages/contracts/src/environment.ts`; `apps/server/src/mcp/OrchestratorMcpService.ts`, `McpInvocationContext.ts`, `linkOrigin.ts`, `toolkits/orchestrator/{tools,handlers}.ts`, `toolkits/thread/tools.ts`, `toolkits/project/{tools,handlers}.ts`, `toolkits/environment/{tools,handlers}.ts` | 28 |

`boundedSnapshotTransport.ts` has no module of its own in v0.1.11; only its
HTTP-level test exists, backed by `threadHistoryPaging.ts` and the shared
bounded-snapshot helpers. The store-level snapshot and compaction cases are
ported in `src/store/stream_tests.rs`; the WebSocket/HTTP framing is skipped
with the transport.

## What is not ported

T3's web UI, Electron shell, mobile apps, relay, pairing and OAuth stay out;
Atrium keeps its own GPUI UI, `atrium-node`, projects, team, memory, workflows
and Office work. Running agents through T3 itself (`docs/t3-extension-plan.md`)
is paused.
