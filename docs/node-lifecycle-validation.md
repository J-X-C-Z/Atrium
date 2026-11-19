# Runtime identity and closed-RPC recovery

This follow-up is based on `96d172dd`. The additive protocol fields and headers
are described in `node-protocol.md`. It changes no GPUI code and persists no
work content, command, stdout, or tombstone.

## Recovery behavior

Each node runtime has a fresh UUID. Spawn/list responses and successful upgrade
headers carry it. A client pins that identity for a process/attached link rather
than replacing it with the latest metadata. An authenticated attach with an old
identity fails with `409 node_restarted` before process lookup, distinguishing
runtime loss from a same-runtime missing/expired process (404) and a transient
network failure (retryable). Old clients and servers without a known identity
keep their existing behavior.

Pinned links refuse a successful upgrade if the response identity is different
or absent. They stop before pumping queued stdin; a confirmed runtime change
never spawns an old command. The bounded 256-entry convenience cache can forget
old IDs, but active links retain independent pins. A caller may explicitly pass
an identity using `attach_with_instance`; no UI or storage migration is made.

This is runtime identification, not process resurrection, guaranteed termination
of escaped descendants, or authentication. Without a previously learned/saved
identity, a new client cannot retrospectively explain why an old process is
absent. Input delivery around network failures remains uncertain, not exactly-
once; existing send-failure queue semantics are not redesigned here.

## Closed-RPC registration race

Independent aggregate testing exposed a pre-existing indefinite wait: the reader
could observe EOF and drain the pending map before a later request registered.
That request then waited forever. A deterministic regression first waits for the
Closed event, then issues a request; the old code exceeded a three-second test
deadline. Closed state and waiter registration now share one mutex. EOF marks
closed and takes pending waiters atomically, then wakes them outside the lock.
A request after closure immediately returns Closed without writing. A 64-round
barrier race also verifies every close/request ordering resolves. These tests
have deadlines instead of hanging CI indefinitely.

## Isolated validation

Linux, Rust 1.100.0, locked dependencies:

```sh
cargo test --locked --offline -p agent-rpc -p codex-client -p acp-client -p atrium-core -p atrium-node
cargo test --locked --offline -p atrium-node --test agents -- --ignored
cargo clippy --locked --offline -p atrium-node -p agent-rpc --all-targets -- -D warnings
cargo clippy --locked --offline -p atrium-node -p agent-rpc --target aarch64-apple-darwin --all-targets -- -D warnings
```

The aggregate command passed 108 tests, with four ignored by that ordinary run;
the explicit native Codex/ACP mock command then passed its two ignored tests.
This includes 13 agent-rpc unit tests, 52 node unit tests, and 3 native protocol
tests. Native lint, macOS-target lint, formatting and diff checks passed. The
macOS-target check is cross-compilation, not a macOS runtime claim.

Covered failure cases include:

- Force-stop only an owned temporary node, queue input during disconnection,
  restart at the same loopback port, and observe old-link Gone/restart rather
  than replay or automatic spawn. Only the fixture's returned cat PIDs are
  checked for EOF termination, never a machine-wide process scan.
- Authentication takes precedence over identity errors; malformed/oversized
  identity is rejected; old-headerless attach still returns normal 404.
- Same-instance automatic reconnect learns and resends its pin, including after
  convenience-cache eviction; queued input can continue on that verified runtime.
- Fake upgrades returning a different identity or dropping identity send zero
  queued input; invalid saved identities fail before network I/O.
- Existing legacy-server metadata and mock clients remain compatible.

No real provider account, user service installation, GUI, or cross-device tailnet
was exercised. These new source changes were not used for the earlier bounded-
load comparison; that report identifies its own executable hashes and scope.
