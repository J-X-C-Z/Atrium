# Upstream: T3 Code

Ported from **T3 Code v0.1.11**, commit `59651c1761f6c63e9b6056839067066968e6a406`
(2026-11-20), MIT. The original notice is in [`LICENSE-T3`](LICENSE-T3).

Every ported file starts with a one-line header naming its T3 source path.
Keep this table current when a module lands or moves.

| `atrium-engine` module | T3 source | Item |
| --- | --- | --- |
| `src/contracts` | `packages/contracts/src` (base schemas, orchestration V2, model selection) | 32 |
| `src/store` | `apps/server/src/orchestration-v2/EventStore.ts`, `ProjectionStore.ts`, `apps/server/src/persistence/Migrations/055_OrchestrationV2.ts` | 32 |
| `src/orchestration` | `apps/server/src/orchestration-v2/Orchestrator.ts`, `EventSink.ts`, `EffectWorker.ts`, `runtimeLayer.ts` | 32 |
| `src/providers` | `packages/provider-core`, `packages/effect-codex-app-server`, `packages/provider-acp` | 25, 26 |
| `src/usage` | `orchestration-v2/UsageLimitRecoveryWorker.ts`, limit schemas | 23 |
| `src/checkpoint` | `apps/server/src/checkpointing`, `orchestration-v2/Checkpoint*Service.ts` | 30 |
| `src/mcp` | `packages/contracts/src/orchestratorMcp.ts`, `apps/server/src/mcp` | 20+ |

## What is not ported

T3's web UI, Electron shell, mobile apps, relay, pairing and OAuth stay out;
Atrium keeps its own GPUI UI, `atrium-node`, projects, team, memory, workflows
and Office work. Running agents through T3 itself (`docs/t3-extension-plan.md`)
is paused.
