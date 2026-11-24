# Upstream

- T3 Code v0.1.11 (MIT), `github.com/pingdotgg/t3code`.
- Claude Agent SDK 0.3.321 (the version T3 0.1.11 pins) for wire shapes.

Ported sources:

- `apps/server/src/provider/claudeUsageLimits.ts` -> `src/usage.rs`
- `apps/server/src/provider/Drivers/ClaudeExecutable.ts` -> `src/exe.rs`
- `apps/server/src/orchestration-v2/Adapters/ClaudeAdapterV2.ts` ->
  `src/args.rs`, `src/client.rs`, `src/events.rs` (result classification,
  spawn/control-pipe structure, prompt-injected effort)
- `packages/shared/src/model.ts` (`applyClaudePromptEffortPrefix`) ->
  `src/events.rs`
- T3's recorded fixtures
  `apps/server/src/orchestration-v2/testkit/fixtures/*/claude_transcript.ndjson`
  -> protocol tests and `mock-claude` scripts
