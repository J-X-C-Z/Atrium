# Workspace notice dispatch integration

Project notice delivery is reserved immediately before a real agent request,
not when the composer is first inspected. The reservation captures the stream,
thread and notice-prefix watermark. It does not persist manual prompt text.

## Sending paths

| Path | Preparation and confirmation |
| --- | --- |
| Existing Codex thread | Local readiness checks, prepare, `turn/start`; nonempty returned turn ID confirms acceptance. |
| Codex steer | Require a client and nonempty current turn before prepare; successful returned ID must equal that turn. |
| Edited Codex message | Keep the original composed message through revert/resume. After success, route to the original thread and prepare once. Failure before this point does not consume notices. |
| New Codex chat | No workstream ticket in Draft; existing thread creation behavior stays unchanged. |
| Existing ACP session | Local readiness/idle checks, prepare, typed `session/prompt` outcome. |
| New ACP chat | No workstream ticket in Draft; existing session creation behavior stays unchanged. |
| Automated parent wake | An explicit project/stream/thread target must produce a Ready ticket before any RPC. Empty sends nothing and clears the wake; Blocked, mismatched target, failed prepare or missing steer turn retain it. Actual attempts use the existing 30-second backoff. |

All transport errors are conservatively uncertain, including standard RPC
refusal codes: an error alone does not establish that no effect or output has
already occurred. No error path retries an RPC or saves the original manual
message for replay. A successful ACP response with a nonempty `stopReason`
confirms acceptance, not successful completion of the requested work.
`prompt_with_outcome` adds this evidence while legacy `prompt` still returns
the same translated closing events.

An unresolved ticket blocks attaching those notices again. Manual messages
remain available, without the held notice prefix, with an existing Notice
message explaining that distinction. Fresh Workspace initialization emits one
summary per project with held deliveries, not a message every polling tick.

`Workspace::resolve_stream_notice_delivery(root, stream, retry_notices, cx)`
is a backend integration entry point requiring an explicit user choice:
`false` marks the captured notices received; `true` authorizes notices-only
retry. There is **no new button, command or GPUI layout** in this change. The
frontend must wire an explicit choice to this API. It is never invoked by an
automatic retry loop. Resolution failure leaves the hold in place.

## Verification boundary

`python3 scripts/test-notice-dispatch.py` builds the real pure dispatch helpers
and Project model without GPUI. Five helper tests cover blocked automatic vs
manual behavior, captured-prefix confirmation, wrong-project/stale-thread
callbacks, persistence failure before dispatch, and Codex success/error ID
classification, stale unread caches after disk edits, and exact automatic target checks. Project fault-injection tests run in the same harness.

`cargo test -p acp-client --offline` includes five loopback response scenarios:
success, standard refusal, output followed by refusal, EOF, and malformed
success. Deadlines are finite and no scenario sends a second prompt or cancel.

These tests do **not** compile the GPUI Workspace glue or exercise actual
window callbacks. Cloud `cargo check -p atrium-app --offline` is blocked before
app compilation by an uncached `accesskit v0.24.1`; macOS app CI remains
necessary. No real model account, system service or second device is used.
