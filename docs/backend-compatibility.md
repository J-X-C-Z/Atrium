# Backend compatibility directions

Reviewed published source: `30a5c029`. Compatibility is directional and scoped;
wire compatibility is not a promise of identical safety semantics or source API.

## Configuration

Existing valid node configurations can load in the new binary. Omitted shared
input and WebSocket limits take their defaults (256 MiB input, 256 total and 2
per-process attachments). Empty roots and existing validation remain in effect.
An old configuration is not a promise that the new node retains unlimited
aggregate resources: new defaults deliberately constrain aggregate admission.

The reverse direction is not supported for newly added keys: the `b14819a`
configuration schema uses `deny_unknown_fields` and rejects, for example,
`total_input_bytes`. This is expected fail-closed behavior, not silent acceptance
with ignored limits. Preserve the matching old configuration with any rollback
binary; do not weaken unknown-field validation to make downgrade appear safe.

The synthetic reproduction linked below uses each revision's actual Config and
Default definitions, with only hostname discovery replaced by a fixed fixture.
It confirms old-key input obtains new defaults and a new input-budget key is
rejected by the old schema. It is a parser compatibility test, not a service
startup or installation test.

## Node protocol and Rust APIs

The v1 NodeInfo/Proc identity fields are optional/defaulted for new readers;
the repository's older serde readers ignore unknown fields. Existing clients
can use ordinary HTTP/WebSocket operations below resource limits. They do not
thereby acquire runtime-identity pinning or the newer application-specific
stdin-refusal terminal semantics. See [protocol details](node-protocol.md).
A new client previously pinned to an instance deliberately refuses an old server
or intermediary that omits the expected successful-upgrade identity. A fresh
unpinned client can retain the legacy fallback; this is not equivalent restart
protection.

Rust downstream code that constructs public structs using exhaustive literals
may need to supply newly added fields or use defaults. JSON compatibility does
not guarantee source compatibility for those callers, nor a stable binary ABI.
The old ACP `prompt` signature and returned closing events remain available;
the typed outcome API is additive and distinguishes confirmed receipt from
uncertain errors without automatic retry.

## Project writers

Old project files load with an empty optional delivery journal. That is **not**
a guarantee that older writers preserve a new file. Any old Project writer
saving a project with an unresolved journal can remove its anti-retry metadata,
even without marking a notice read. Do not mix old/new writers on one project
or downgrade with unresolved entries. Older node HTTP clients do not themselves
write this Project field. Full scope and the confirmed `37c86bfe` → `30a5c029`
synthetic reproduction are in [notice-delivery recovery](notice-delivery-recovery.md#mixed-version-writers-are-unsupported-not-just-rollback).

[Reproduction code and observations](evidence/notice-mixed-writer) use only
synthetic temporary data, no provider accounts, service registrations, or user
projects. They demonstrate the unsafe write sequence, not execution of duplicate
remote work. The application does not enforce a project format-version fence.
