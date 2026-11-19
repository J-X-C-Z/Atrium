# Bounded RPC line reception

The shared Codex/ACP JSON-RPC reader accepts at most **64 MiB (67,108,864
bytes) before LF**. An optional CR counts toward this limit. Thus exactly
64 MiB followed by LF is accepted; with CRLF, the JSON bytes plus CR must
fit within 64 MiB. An EOF-terminated final line is accepted within the same
limit. As with the former `BufRead::lines`, CR is stripped only before LF,
not from an EOF-only final line.

## Compatibility calculation

`atrium-node` limits the original process-output line to 64 MiB before LF,
including CR, and also bounds its UTF-8-decoded output. Its WebSocket JSON
wrapper can expand characters through escaping (up to roughly six times the
line size plus framing metadata). That wrapper is **not** the input to the
RPC reader: `NodeClient` decodes `frame["line"]` first and then appends LF.
The RPC reader therefore sees the original agent JSON line, not the wrapper.
A 384 MiB RPC line allowance is unnecessary to support legal node output.

Local Codex app-server and ACP stdio use the same newline-delimited reader.
They previously had no explicit receive-line ceiling in this client. This
change introduces the same 64 MiB limit for them; a larger local-agent message
now closes the RPC reader. Public client APIs and wire formats are unchanged.

## Failure behavior and scope

The reader checks each borrowed input chunk before appending. Buffer growth
requests never exceed the line limit. An overlong line, invalid UTF-8 or input
I/O error ends the read loop. It does not truncate, skip to the next line, or
process subsequent responses/notifications from the same input. The existing
closed-state transition resolves every pending request with `RpcError::Closed`
and queues `Incoming::Closed`. Future requests observe the closed state.
Within-limit malformed JSON retains the earlier log-and-skip behavior.

This bounds the line accumulator, **not total process RSS**, the upstream
WebSocket/pipe buffers, parsed JSON objects or event queues. There is no new
synchronous queue backpressure, automatic retry, process kill or timeout for
a peer that sends a short incomplete line and then remains idle.

## Verification

```sh
cargo test -p agent-rpc --offline
cargo clippy -p agent-rpc --all-targets --offline -- -D warnings
```

Four new tests use injectable small limits instead of allocating a 64 MiB
fixture. Each has a finite three-second deadline:

- Exact-limit LF, CRLF and EOF boundaries, including EOF-only trailing CR.
- Unterminated overflow, verifying offending bytes were not appended/consumed.
- Overflow, invalid UTF-8 and EOF close all three registered waiters. A valid
  response and notification placed after invalid input are never routed.
- A 32 KiB exact-limit JSON line with Unicode/escaped data, split across small
  reader chunks, still routes normally; preceding malformed JSON is skipped.

At this revision, the complete agent-rpc run passes 22 unit tests and four
integration tests; one test requiring a separately running node is ignored.
These are isolated fixtures, without real agent credentials or accounts.
