# Native mock hierarchy acceptance (headless)

This exercises the production project model, the production wake predicates
shared with the workspace, and the native node + existing remote Codex and ACP
clients. It runs both an all-Codex tree and one with a Claude/ACP lead. It does
not require provider accounts, a display, system-service installation, or
changes to Tailscale or existing settings.

```sh
cargo build --locked -p codex-client --bin mock-codex -p acp-client --bin mock-acp
cargo test --locked -p atrium-node --test agents -- --ignored
```

The tests build/use real clients and listen on a temporary localhost port with
a temporary root and mock-only agent definitions. They isolate the node HOME
and disable Magpie discovery. The helper mode is set only for the hierarchy
test's own node and applies only to repository mock binaries, not actual
providers. It prevents prose in kickoffs from accidentally triggering the
mocks' interactive demonstration cases, and echoes the full wake content so
the assertions can establish the actual message received by the mock.

The fixture writes controller/lead task files after the previous mock turn
completes, like an agent writing them. Project code checks lead approval,
derives lead and worker roles and ancestry, claims tasks, persists bindings,
formats kickoffs, reports completions, deduplicates receipts, recovers pending
wake recipients after reopening the project, and formats unread notices. The
fixture checks the shared wake predicates and sends those notices through the
existing remote clients, then checks that each parent's next turn completes
and returns the expected result text. The fixture rewrites the task result
file, drives tick/dispatch timing, and saves the read cursor; neither built-in
mock autonomously edits project files in this scenario.

**Not covered:** live Workspace async scheduling and View/GPUI dispatch, real
agents, remote Tailnet, or service-manager startup. The exact same headless
test is picked up by the existing CI step running ignored `agents` tests
after building mock binaries. CI must actually pass before claiming coverage
on its platform.
