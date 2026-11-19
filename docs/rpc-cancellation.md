# Local RPC request cancellation cleanup

Scope: `agent-rpc/src/rpc.rs`, preserving all public APIs and wire messages.
Dropping a request future releases its local pending response sender. It does
**not** cancel remote execution, send an interrupt, retry a request, or retract
bytes already written to an agent or queued in an attachment.

## Failure reproduced

With an open pipe whose server never replies, poll a request once, verify its
outgoing request and the single registered waiter, then drop the future. The
old code leaves the waiter in the map until a reply or EOF. The new deterministic
regression failed on that exact nonempty-map assertion before the fix. A bounded
three-second test wrapper prevents a regression from hanging the suite.

## Implementation

After atomic closed-state checking/registration, the request owns a private RAII
registration guard before its first await. Guard destruction removes only that
request ID. Normal completion, late response, EOF and cancellation can race:
removing an already absent entry is a no-op. Any removed sender is dropped after
the mutex is released. No cancellation-history set is retained.

Parameter serialization remains before registration. Write/flush failures use
the same guard cleanup as cancellation. The separate EOF fix retains its
atomic `closed` flag and drains outstanding waiters outside the shared lock.

## Verification

```sh
cargo test -p agent-rpc --offline
cargo clippy -p agent-rpc --all-targets --offline -- -D warnings
```

Verified on the isolated Linux workspace: **17 unit tests and 4 integration
tests pass**, with one real-node test intentionally ignored. Added coverage:

- 128 poll/drop cancellations with the server kept open: zero pending entries
  after each drop and no extra cancel/interrupt/retry bytes.
- Cancelling one of two requests preserves the other; a late cancelled response
  is ignored and the surviving request receives its own result.
- 64 barrier-coordinated cancellation/response/EOF races with finite deadlines.
- Serialization and write failures leave no pending entry.

This does not bound never-cancelled requests, transport queues, blocking writes,
or remote agent work. Those require independent resource controls or deadlines.
