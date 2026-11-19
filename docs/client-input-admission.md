# Client attachment input admission

This change bounds `NodeClient` attachment input locally. It is fail-fast
admission, not a blocking queue or remote acknowledgement. The public `Write`
interface and node wire messages remain unchanged.

## Defaults and accounting

- One raw/decoded line: 64 MiB excluding LF, matching node stdin admission.
- One attachment: 128 MiB and 32 logical messages.
- One `NodeClient` and its clones: 256 MiB and 1,024 logical messages shared
  across attachments. Separately constructed clients have separate budgets.

A 64 MiB line plus LF fits when the budgets are otherwise empty. The local
32-message ceiling matches the node's existing per-process input queue count.
The shared count allows 32 attachments each to hold that many small messages,
above the scheduler's 12-running-workstream cap; bytes can limit admission
first. These are initial safety ceilings, not workload-optimal benchmark claims.

Partial lines reserve one message slot. Completed lines, channel entries,
reconnect backlog and the line currently being sent keep their RAII credits.
Taking a line from a channel does not release its credit. UTF-8 replacement
expansion is calculated without allocating the expanded string and reserved
before any new line from that write call enters the channel. LF is included
in byte accounting. Finished raw buffers are dropped instead of retaining a
large empty vector behind already-released credits.

The byte budget measures retained input payload, not whole-process RSS. Input
scan/conversion buffers, WebSocket escaping/serialization, TCP buffers and
output channels are separate. During conversion, the complete raw write buffer
can coexist with strings already handed to the channel; a partial vector's
capacity can exceed its logical length. Those temporary copies and spare
capacity are not additional charged payload. JSON wrapping can expand the
in-flight line. This is not an exact cap on all allocated input bytes.
No outbound delivery acknowledgement or output-channel backpressure is added.

## Write failure and terminal EOF

A call first reserves its raw bytes, then scans/counts logical lines and
reserves slots. Oversized calls are rejected before copying. Completed decoded
lines are checked before enqueueing any new line in the call. Length/quota
preflight failure therefore enqueues none of that call's new complete lines.
Earlier calls may already have been delivered. A channel failure during the
send loop can occur after some complete lines were enqueued, so its error
explicitly leaves prior delivery uncertain.

Any local refusal closes this attachment, drops its unfinished fragment and
its only input sender, and revokes the unique output sender. Dropping the
input sender wakes reconnect backoff without waiting for its timeout; revoking
output makes EOF available without waiting for a stalled connect/handshake.
Already buffered output is still drained before EOF. The current failing RPC
returns `Write`; other unanswered requests close through normal RPC EOF, and
later requests observe `Closed`. No cancel or kill is sent to the agent.

A network operation already committed to sending can race local refusal; that
input may have been delivered. No subsequent retry is initiated by quota
failure. Terminal state cannot be overwritten by a later connect result.
An actually blocked network worker can still own a bounded channel/backlog
or in-flight line until it returns and drops them. Their credits remain charged
until then; the implementation does not pretend those bytes were reclaimed.
This slice does not interrupt OS connect, kill worker threads or make ordinary
writer Drop interrupt a stalled handshake.

`flush()` remains a local no-op for a healthy attachment, not confirmation that
a command reached the node. After local admission refusal, it returns an error.
There is no capacity wait while the RPC connection's writer mutex is held.

## Existing uncertain-delivery policy is unchanged

The old replay loop uses `queued.drain(..)`: a socket-send error drops that
failed replay line **and the remaining drained backlog**. In contrast, a send
failure while consuming a live channel line puts that line in the reconnect
queue. This implementation deliberately preserves both behaviors; an explicit
regression exercises them on a locally shut-down socket. It does not silently
fix the former loss or promise that the latter retry cannot duplicate effects.
Changing those policies requires a separate delivery-contract decision.
Runtime-instance pinning and terminal server rejection behavior remain intact.

## Isolated verification

The isolated implementation was built from `d9751e11` in its own checkout and target directory.
Tests use small injectable limits rather than allocating maximum-size frames.
They cover fragment/channel/dequeued accounting, shared-client rejection,
multiline atomic preflight, UTF-8 expansion, retained-buffer release, actual
NodeClient/Link + RPC EOF during repeated HTTP 429 and deliberately stalled
WebSocket handshake, a late successful 101 upgrade after quota termination
(no old input sent and no revival of Connected), and the unchanged two
send-failure behaviors. Network
fixtures have finite deadlines and use only owned loopback sockets.

The existing reconnect, identity pin, RPC cancellation, ACP and Codex tests
are also run. These tests do not measure throughput improvements or prove
whole-process memory bounds. The final source passed 30 agent-rpc unit tests and 4 integration tests,
including the late successful WebSocket-upgrade case. Full ACP/Codex and
native node/mock regressions passed on Linux. Exact-head macOS CI is tracked
separately; this is not real-device, provider or GUI acceptance.
